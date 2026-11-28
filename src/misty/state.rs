//! Sessions, messages, finished runs and environments: `<home>/misty/state.db`. Local only; nothing here is sent anywhere.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::env::Environment;
use super::feedback::{Label, Verdict, CAPACITY};

pub struct Store {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// An environment from its row. A row whose settings do not parse still comes back, by its name, so it can be seen and
/// deleted.
fn read_environment(r: &rusqlite::Row) -> Environment {
    let text = |i: usize| r.get::<_, String>(i).unwrap_or_default();
    let mut e: Environment = serde_json::from_str(&text(2)).unwrap_or_default();
    e.id = text(0);
    e.name = text(1);
    e.created = r.get(3).unwrap_or(0);
    e.updated = r.get(4).unwrap_or(0);
    e
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        Self::init(Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    pub fn memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, title TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
               role TEXT NOT NULL, content TEXT NOT NULL, ts INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, id);
             CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY, session_id TEXT, input TEXT NOT NULL, output TEXT NOT NULL, role TEXT,
               status TEXT NOT NULL, started INTEGER NOT NULL, ms INTEGER NOT NULL, events TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS environments(id TEXT PRIMARY KEY, name TEXT NOT NULL, spec TEXT NOT NULL,
               created INTEGER NOT NULL, updated INTEGER NOT NULL);
             CREATE UNIQUE INDEX IF NOT EXISTS environments_name ON environments(name COLLATE NOCASE);
             CREATE TABLE IF NOT EXISTS feedback(id INTEGER PRIMARY KEY, question TEXT NOT NULL, answer TEXT NOT NULL,
               label TEXT NOT NULL, tools TEXT NOT NULL, correction TEXT, at INTEGER NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        // A session's environment and folder came after the first version of this file: add the columns to an old one.
        for col in ["env_id", "cwd"] {
            let has = conn
                .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?1")
                .and_then(|mut q| q.exists(params![col]))
                .map_err(|e| e.to_string())?;
            if !has {
                conn.execute(&format!("ALTER TABLE sessions ADD COLUMN {col} TEXT"), [])
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    // ---- environments and what a session works in ------------------------------------------------------------------

    /// Every environment, by name.
    pub fn environments(&self) -> Result<Vec<Environment>, String> {
        let c = self.conn.lock();
        let mut st = c
            .prepare("SELECT id, name, spec, created, updated FROM environments ORDER BY name COLLATE NOCASE")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok(read_environment(r)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// One environment, by id or else by name (any case).
    pub fn environment(&self, key: &str) -> Result<Option<Environment>, String> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id, name, spec, created, updated FROM environments WHERE id = ?1 OR name = ?1 COLLATE NOCASE
             ORDER BY id = ?1 DESC LIMIT 1",
            params![key],
            |r| Ok(read_environment(r)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    /// Adds `e` (no id yet) or replaces the one with its id. Two environments never share a name (any case).
    pub fn save_environment(&self, e: &mut Environment) -> Result<(), String> {
        let c = self.conn.lock();
        let clash: Option<String> = c
            .query_row(
                "SELECT id FROM environments WHERE name = ?1 COLLATE NOCASE AND id != ?2",
                params![e.name, e.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if clash.is_some() {
            return Err(format!("an environment named {:?} already exists", e.name));
        }
        let t = now();
        if e.id.is_empty() {
            e.id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
            e.created = t;
            e.updated = t;
            let spec = serde_json::to_string(e).map_err(|x| x.to_string())?;
            c.execute(
                "INSERT INTO environments(id, name, spec, created, updated) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![e.id, e.name, spec, t],
            )
            .map_err(|x| x.to_string())?;
        } else {
            e.updated = t;
            let spec = serde_json::to_string(e).map_err(|x| x.to_string())?;
            let n = c
                .execute(
                    "UPDATE environments SET name = ?2, spec = ?3, updated = ?4 WHERE id = ?1",
                    params![e.id, e.name, spec, t],
                )
                .map_err(|x| x.to_string())?;
            if n == 0 {
                return Err(format!("no environment {}", e.id));
            }
        }
        Ok(())
    }

    /// Removes an environment; the sessions that used it keep their own folder and go back to Misty's policy.
    pub fn delete_environment(&self, id: &str) -> Result<bool, String> {
        let c = self.conn.lock();
        c.execute("UPDATE sessions SET env_id = NULL WHERE env_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(c.execute("DELETE FROM environments WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?
            > 0)
    }

    /// The environment id and working folder a session has (`None`: none chosen).
    pub fn session_scope(&self, session: &str) -> Result<(Option<String>, Option<String>), String> {
        self.conn
            .lock()
            .query_row(
                "SELECT env_id, cwd FROM sessions WHERE id = ?1",
                params![session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map(Option::unwrap_or_default)
            .map_err(|e| e.to_string())
    }

    /// Sets both values (`None` clears one). False when there is no such session.
    pub fn set_session_scope(&self, session: &str, env: Option<&str>, cwd: Option<&str>) -> Result<bool, String> {
        Ok(self
            .conn
            .lock()
            .execute(
                "UPDATE sessions SET env_id = ?2, cwd = ?3 WHERE id = ?1",
                params![session, env, cwd],
            )
            .map_err(|e| e.to_string())?
            > 0)
    }

    pub fn new_session(&self, title: &str) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let t = now();
        self.conn
            .lock()
            .execute(
                "INSERT INTO sessions(id, title, created, updated) VALUES (?1, ?2, ?3, ?3)",
                params![id, title, t],
            )
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn has_session(&self, id: &str) -> bool {
        self.conn
            .lock()
            .query_row("SELECT 1 FROM sessions WHERE id = ?1", params![id], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<bool, String> {
        Ok(self
            .conn
            .lock()
            .execute("UPDATE sessions SET title = ?2 WHERE id = ?1", params![id, title])
            .map_err(|e| e.to_string())?
            > 0)
    }

    pub fn append(&self, session: &str, role: &str, content: &str) -> Result<(), String> {
        let c = self.conn.lock();
        c.execute(
            "INSERT INTO messages(session_id, role, content, ts) VALUES (?1, ?2, ?3, ?4)",
            params![session, role, content, now()],
        )
        .map_err(|e| e.to_string())?;
        c.execute(
            "UPDATE sessions SET updated = ?2 WHERE id = ?1",
            params![session, now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The last `limit` messages of a session, oldest first.
    pub fn history(&self, session: &str, limit: usize) -> Result<Vec<(String, String)>, String> {
        let c = self.conn.lock();
        let mut st = c
            .prepare("SELECT role, content FROM (SELECT id, role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![session, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn sessions(&self, limit: usize) -> Result<Vec<Value>, String> {
        let c = self.conn.lock();
        let mut st = c
            .prepare(
                "SELECT s.id, s.title, s.created, s.updated, (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id),
                        s.env_id, e.name, s.cwd
                 FROM sessions s LEFT JOIN environments e ON e.id = s.env_id
                 ORDER BY s.updated DESC, s.created DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![limit as i64], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "created": r.get::<_, i64>(2)?,
                          "updated": r.get::<_, i64>(3)?, "messages": r.get::<_, i64>(4)?,
                          "env": r.get::<_, Option<String>>(5)?, "env_name": r.get::<_, Option<String>>(6)?,
                          "cwd": r.get::<_, Option<String>>(7)?}))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn delete_session(&self, id: &str) -> Result<bool, String> {
        let c = self.conn.lock();
        c.execute("UPDATE runs SET session_id = NULL WHERE session_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(c.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?
            > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_run(
        &self,
        id: &str,
        session: Option<&str>,
        input: &str,
        output: &str,
        role: Option<&str>,
        status: &str,
        started: i64,
        ms: u64,
        events: &[Value],
    ) -> Result<(), String> {
        self.conn
            .lock()
            .execute(
                "INSERT OR REPLACE INTO runs(id, session_id, input, output, role, status, started, ms, events) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, session, input, output, role, status, started, ms as i64, serde_json::to_string(events).unwrap_or_default()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn runs(&self, limit: usize) -> Result<Vec<Value>, String> {
        let c = self.conn.lock();
        let mut st = c
            .prepare("SELECT id, session_id, substr(input, 1, 200), role, status, started, ms FROM runs ORDER BY started DESC, rowid DESC LIMIT ?1")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![limit as i64], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "session": r.get::<_, Option<String>>(1)?, "input": r.get::<_, String>(2)?,
                          "role": r.get::<_, Option<String>>(3)?, "status": r.get::<_, String>(4)?, "started": r.get::<_, i64>(5)?,
                          "ms": r.get::<_, i64>(6)?}))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn run(&self, id: &str) -> Result<Option<Value>, String> {
        self.conn
            .lock()
            .query_row("SELECT id, session_id, input, output, role, status, started, ms, events FROM runs WHERE id = ?1", params![id], |r| {
                let events: String = r.get(8)?;
                Ok(json!({"id": r.get::<_, String>(0)?, "session": r.get::<_, Option<String>>(1)?, "input": r.get::<_, String>(2)?,
                          "output": r.get::<_, String>(3)?, "role": r.get::<_, Option<String>>(4)?, "status": r.get::<_, String>(5)?,
                          "started": r.get::<_, i64>(6)?, "ms": r.get::<_, i64>(7)?,
                          "events": serde_json::from_str::<Value>(&events).unwrap_or(Value::Null)}))
            })
            .optional()
            .map_err(|e| e.to_string())
    }

    // ---- the owner's verdicts on replies ----------------------------------------------------------------------------

    /// Saves a verdict, then keeps only the newest [`CAPACITY`] of them. Returns its id.
    pub fn save_feedback(&self, v: &Verdict) -> Result<i64, String> {
        let c = self.conn.lock();
        let tools = serde_json::to_string(&v.tools).map_err(|e| e.to_string())?;
        c.execute(
            "INSERT INTO feedback(question, answer, label, tools, correction, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![v.question, v.answer, v.label.wire(), tools, v.correction, v.at],
        )
        .map_err(|e| e.to_string())?;
        let id = c.last_insert_rowid();
        c.execute(
            "DELETE FROM feedback WHERE id NOT IN (SELECT id FROM feedback ORDER BY id DESC LIMIT ?1)",
            params![CAPACITY as i64],
        )
        .map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// Every saved verdict with its id, oldest first. A row with an unknown label is left out.
    pub fn feedback(&self) -> Result<Vec<(i64, Verdict)>, String> {
        let c = self.conn.lock();
        let mut st = c
            .prepare("SELECT id, question, answer, label, tools, correction, at FROM feedback ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = vec![];
        for row in rows {
            let (id, question, answer, label, tools, correction, at) = row.map_err(|e| e.to_string())?;
            let Some(label) = Label::of(&label) else { continue };
            let tools: Vec<String> = serde_json::from_str(&tools).unwrap_or_default();
            out.push((
                id,
                Verdict {
                    question,
                    answer,
                    label,
                    tools,
                    correction,
                    at,
                },
            ));
        }
        Ok(out)
    }

    /// Every saved verdict, oldest first.
    pub fn feedback_all(&self) -> Result<Vec<Verdict>, String> {
        Ok(self.feedback()?.into_iter().map(|(_, v)| v).collect())
    }

    /// Removes one verdict. False when there is no such id.
    pub fn forget_feedback(&self, id: i64) -> Result<bool, String> {
        Ok(self
            .conn
            .lock()
            .execute("DELETE FROM feedback WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?
            > 0)
    }

    /// Removes every verdict. Returns how many there were.
    pub fn clear_feedback(&self) -> Result<usize, String> {
        self.conn
            .lock()
            .execute("DELETE FROM feedback", [])
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_keep_their_messages_in_order_and_cascade_on_delete() {
        let s = Store::memory().unwrap();
        let a = s.new_session("first").unwrap();
        let b = s.new_session("second").unwrap();
        for i in 0..5 {
            s.append(&a, if i % 2 == 0 { "user" } else { "assistant" }, &format!("m{i}"))
                .unwrap();
        }
        s.append(&b, "user", "other").unwrap();
        let h = s.history(&a, 3).unwrap();
        assert_eq!(
            h.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            vec!["m2", "m3", "m4"]
        );
        assert_eq!(s.history(&b, 10).unwrap().len(), 1);
        assert!(s.has_session(&a) && !s.has_session("nope"));
        let list = s.sessions(10).unwrap();
        assert_eq!(list.len(), 2);
        assert!(s.rename(&a, "renamed").unwrap());
        assert!(s.delete_session(&a).unwrap());
        assert!(s.history(&a, 10).unwrap().is_empty());
        assert!(!s.delete_session(&a).unwrap());
    }

    fn env(name: &str) -> Environment {
        Environment {
            name: name.into(),
            cwd: Some("/work/a".into()),
            ..Default::default()
        }
    }

    #[test]
    fn environments_are_saved_replaced_found_by_id_or_name_and_deleted() {
        let s = Store::memory().unwrap();
        let mut a = env("Dev box");
        a.vars.insert("MODE".into(), "dev".into());
        s.save_environment(&mut a).unwrap();
        assert_eq!(a.id.len(), 12);
        assert!(a.created > 0 && a.created == a.updated);
        let mut b = env("alpha");
        s.save_environment(&mut b).unwrap();
        // by name, any order and case, then by id
        let names: Vec<String> = s.environments().unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["alpha", "Dev box"]);
        let got = s.environment("DEV BOX").unwrap().unwrap();
        assert_eq!((got.id.as_str(), got.vars["MODE"].as_str()), (a.id.as_str(), "dev"));
        assert_eq!(s.environment(&b.id).unwrap().unwrap().name, "alpha");
        assert!(s.environment("nope").unwrap().is_none());
        // replace: same id, new settings
        let mut again = got;
        again.cwd = Some("/work/b".into());
        again.name = "Dev box 2".into();
        s.save_environment(&mut again).unwrap();
        assert_eq!(s.environments().unwrap().len(), 2);
        assert_eq!(s.environment(&a.id).unwrap().unwrap().cwd.as_deref(), Some("/work/b"));
        // a name is never shared, whatever the case; renaming to its own name is fine
        assert!(s
            .save_environment(&mut env("ALPHA"))
            .unwrap_err()
            .contains("already exists"));
        let mut same = s.environment(&b.id).unwrap().unwrap();
        s.save_environment(&mut same).unwrap();
        // a replace of an environment that is gone says so
        let mut ghost = env("ghost");
        ghost.id = "000000000000".into();
        assert!(s.save_environment(&mut ghost).unwrap_err().contains("no environment"));
        assert!(s.delete_environment(&a.id).unwrap() && !s.delete_environment(&a.id).unwrap());
        assert_eq!(s.environments().unwrap().len(), 1);
    }

    #[test]
    fn a_session_remembers_its_environment_and_folder_and_loses_the_environment_when_it_is_deleted() {
        let s = Store::memory().unwrap();
        let mut e = env("work");
        s.save_environment(&mut e).unwrap();
        let sid = s.new_session("t").unwrap();
        assert_eq!(s.session_scope(&sid).unwrap(), (None, None));
        assert!(s.set_session_scope(&sid, Some(&e.id), Some("/work/own")).unwrap());
        assert_eq!(
            s.session_scope(&sid).unwrap(),
            (Some(e.id.clone()), Some("/work/own".to_string()))
        );
        let listed = &s.sessions(10).unwrap()[0];
        assert_eq!(
            (
                listed["env"].as_str(),
                listed["env_name"].as_str(),
                listed["cwd"].as_str()
            ),
            (Some(e.id.as_str()), Some("work"), Some("/work/own"))
        );
        assert!(s.delete_environment(&e.id).unwrap());
        assert_eq!(s.session_scope(&sid).unwrap(), (None, Some("/work/own".to_string())));
        assert!(!s.set_session_scope("nope", None, None).unwrap());
        assert_eq!(s.session_scope("nope").unwrap(), (None, None));
    }

    #[test]
    fn a_state_file_from_before_environments_is_upgraded_and_keeps_its_sessions() {
        let d = crate::testing::TestDir::new("misty-state-old");
        let path = d.join("state.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT NOT NULL, created INTEGER NOT NULL, updated INTEGER NOT NULL);
                 INSERT INTO sessions VALUES ('old1', 'before', 1, 2);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let listed = s.sessions(10).unwrap();
        assert_eq!(listed[0]["title"], "before");
        assert!(listed[0]["env"].is_null() && listed[0]["cwd"].is_null());
        assert!(s.set_session_scope("old1", None, Some("/x")).unwrap());
        drop(s);
        // opening it a second time does not try to add the columns again
        let s = Store::open(&path).unwrap();
        assert_eq!(s.session_scope("old1").unwrap().1.as_deref(), Some("/x"));
    }

    #[test]
    fn runs_are_saved_listed_and_read_back_with_their_events() {
        let s = Store::memory().unwrap();
        let sid = s.new_session("t").unwrap();
        s.save_run(
            "r1",
            Some(&sid),
            "hello",
            "hi",
            Some("fast"),
            "done",
            100,
            42,
            &[json!({"type": "route"}), json!({"type": "final"})],
        )
        .unwrap();
        let list = s.runs(10).unwrap();
        assert_eq!(list[0]["id"], "r1");
        let r = s.run("r1").unwrap().unwrap();
        assert_eq!(r["events"].as_array().unwrap().len(), 2);
        assert_eq!(r["output"], "hi");
        assert!(s.run("missing").unwrap().is_none());
        s.delete_session(&sid).unwrap();
        assert!(s.run("r1").unwrap().is_some(), "a deleted session keeps its run record");
    }

    #[test]
    fn verdicts_are_saved_listed_forgotten_and_only_the_newest_are_kept() {
        let s = Store::memory().unwrap();
        let v = Verdict::new("what time is it", "noon", Label::NoTools, vec![], None, 1).unwrap();
        let id = s.save_feedback(&v).unwrap();
        let fix = Verdict::new(
            "capital of france",
            "Lyon",
            Label::Corrected,
            vec!["calc".into()],
            Some("Paris"),
            2,
        )
        .unwrap();
        s.save_feedback(&fix).unwrap();
        let rows = s.feedback().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (id, v.clone()));
        assert_eq!(rows[1].1.correction.as_deref(), Some("Paris"));
        assert_eq!(rows[1].1.tools, vec!["calc".to_string()]);
        assert!(s.forget_feedback(id).unwrap());
        assert!(!s.forget_feedback(id).unwrap());
        assert_eq!(s.feedback_all().unwrap().len(), 1);
        // only the newest CAPACITY are kept
        for n in 0..CAPACITY + 3 {
            let x = Verdict::new(&format!("question {n}"), "a", Label::Bad, vec![], None, n as i64).unwrap();
            s.save_feedback(&x).unwrap();
        }
        let all = s.feedback_all().unwrap();
        assert_eq!(all.len(), CAPACITY);
        assert_eq!(all.last().unwrap().question, format!("question {}", CAPACITY + 2));
        assert_eq!(s.clear_feedback().unwrap(), CAPACITY);
        assert!(s.feedback_all().unwrap().is_empty());
    }
}

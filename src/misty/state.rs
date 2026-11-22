//! Sessions, messages and finished runs: `<home>/misty/state.db`. Local only; nothing here is sent anywhere.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub struct Store {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
               status TEXT NOT NULL, started INTEGER NOT NULL, ms INTEGER NOT NULL, events TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Mutex::new(conn) })
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
                "SELECT s.id, s.title, s.created, s.updated, (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
                 FROM sessions s ORDER BY s.updated DESC, s.created DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![limit as i64], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "created": r.get::<_, i64>(2)?,
                          "updated": r.get::<_, i64>(3)?, "messages": r.get::<_, i64>(4)?}))
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
}

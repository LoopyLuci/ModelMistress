//! The KM store: folders under `<home>/km/store/<id>@<version>/` and a SQLite index (`<home>/km/index.db`) over them.
//!
//! The index holds the manifest fields the selector needs, a `tags` table, an FTS5 table over name, description,
//! descriptor and keywords, the centroid (f16 bytes) with the embedder that made it, the channels (a channel move is
//! one transaction), the owner's pins and bans, and a history of imports, removals and channel moves. The folders are
//! immutable once installed; the index can be rebuilt from them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use semver::Version;
use serde::Serialize;
use serde_json::{json, Value};

use super::manifest::Manifest;
use super::pack::{self, SigState, Source};
use super::{now_ms, Res};
use crate::engine::Fail;

fn db<E: std::fmt::Display>(e: E) -> Fail {
    Fail::new(500, "km_index", format!("KM index: {e}"))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS km (
  key TEXT PRIMARY KEY, id TEXT NOT NULL, version TEXT NOT NULL, content_id TEXT NOT NULL,
  kind TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL, domain TEXT NOT NULL,
  uuid TEXT, parent TEXT, license TEXT,
  tags TEXT NOT NULL, base TEXT NOT NULL, base_version TEXT, arch TEXT, base_hash TEXT,
  input TEXT NOT NULL, output TEXT NOT NULL,
  descriptor TEXT NOT NULL, keywords TEXT NOT NULL, embed_model TEXT, prior REAL NOT NULL,
  centroid BLOB, ram_bytes INTEGER, vram_bytes INTEGER, bytes INTEGER NOT NULL,
  source TEXT NOT NULL, sig_state TEXT NOT NULL, signed_by TEXT, overridden INTEGER NOT NULL DEFAULT 0,
  misdeclared TEXT, installed_at INTEGER NOT NULL,
  requires TEXT NOT NULL, conflicts TEXT NOT NULL, consolidation TEXT NOT NULL, tag_info TEXT NOT NULL,
  manifest TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS km_id ON km(id);
CREATE TABLE IF NOT EXISTS tags (
  key TEXT NOT NULL, tag TEXT NOT NULL, origin TEXT NOT NULL DEFAULT 'manual', confidence REAL NOT NULL DEFAULT 1.0,
  PRIMARY KEY (key, tag)
);
CREATE INDEX IF NOT EXISTS tags_tag ON tags(tag);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(key UNINDEXED, name, description, descriptor, keywords);
CREATE TABLE IF NOT EXISTS channels (
  id TEXT NOT NULL, channel TEXT NOT NULL, version TEXT NOT NULL, moved_at INTEGER NOT NULL,
  PRIMARY KEY (id, channel)
);
CREATE TABLE IF NOT EXISTS owner (
  id TEXT PRIMARY KEY, pinned INTEGER NOT NULL DEFAULT 0, banned INTEGER NOT NULL DEFAULT 0,
  note TEXT, at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS history (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, action TEXT NOT NULL, key TEXT NOT NULL, detail TEXT
);
";

const COLS: &str = "key, id, version, content_id, kind, name, description, domain, uuid, parent, license, tags, base, \
    base_version, arch, base_hash, input, output, descriptor, keywords, embed_model, prior, ram_bytes, vram_bytes, \
    bytes, source, sig_state, signed_by, overridden, misdeclared, installed_at, requires, conflicts, consolidation, \
    tag_info";

/// One installed KM version, as the index knows it.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub key: String,
    pub id: String,
    pub version: String,
    pub content_id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub domain: String,
    pub uuid: Option<String>,
    pub parent: Option<String>,
    pub license: Option<String>,
    pub tags: Vec<String>,
    pub base: String,
    pub base_version: Option<String>,
    pub arch: Option<String>,
    pub base_hash: Option<String>,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub descriptor: String,
    pub keywords: Vec<String>,
    pub embed_model: Option<String>,
    pub prior: f64,
    pub ram_bytes: Option<u64>,
    pub vram_bytes: Option<u64>,
    pub bytes: u64,
    pub source: String,
    pub signature: String,
    pub signed_by: Option<String>,
    pub overridden: bool,
    /// Set when a measured load exceeded the declared resources by more than 10%.
    pub misdeclared: Option<String>,
    pub installed_at: i64,
    pub requires: Vec<String>,
    pub conflicts: Vec<String>,
    /// Per core: `{state, uplift, last_step, steps}`.
    pub consolidation: Value,
    /// Per tag: `{origin, confidence}`.
    pub tag_info: Value,
    #[serde(skip)]
    pub centroid: Option<Vec<u8>>,
}

impl Record {
    pub fn version(&self) -> Version {
        Version::parse(&self.version).unwrap_or_else(|_| Version::new(0, 0, 0))
    }
    pub fn summary(&self) -> Value {
        json!({"key": self.key, "id": self.id, "version": self.version, "kind": self.kind, "name": self.name,
               "description": self.description, "domain": self.domain, "tags": self.tags,
               "bytes": self.bytes, "content_id": self.content_id, "signature": self.signature,
               "source": self.source, "misdeclared": self.misdeclared, "has_centroid": self.centroid.is_some()
                   || self.embed_model.is_some()})
    }
}

fn jl(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}
fn lj(s: String) -> Vec<String> {
    serde_json::from_str(&s).unwrap_or_default()
}

fn record_of(r: &Row, centroid: Option<Vec<u8>>) -> rusqlite::Result<Record> {
    Ok(Record {
        key: r.get(0)?,
        id: r.get(1)?,
        version: r.get(2)?,
        content_id: r.get(3)?,
        kind: r.get(4)?,
        name: r.get(5)?,
        description: r.get(6)?,
        domain: r.get(7)?,
        uuid: r.get(8)?,
        parent: r.get(9)?,
        license: r.get(10)?,
        tags: lj(r.get(11)?),
        base: r.get(12)?,
        base_version: r.get(13)?,
        arch: r.get(14)?,
        base_hash: r.get(15)?,
        input: lj(r.get(16)?),
        output: lj(r.get(17)?),
        descriptor: r.get(18)?,
        keywords: lj(r.get(19)?),
        embed_model: r.get(20)?,
        prior: r.get(21)?,
        ram_bytes: r.get::<_, Option<i64>>(22)?.map(|v| v as u64),
        vram_bytes: r.get::<_, Option<i64>>(23)?.map(|v| v as u64),
        bytes: r.get::<_, i64>(24)? as u64,
        source: r.get(25)?,
        signature: r.get(26)?,
        signed_by: r.get(27)?,
        overridden: r.get::<_, i64>(28)? != 0,
        misdeclared: r.get(29)?,
        installed_at: r.get(30)?,
        requires: lj(r.get(31)?),
        conflicts: lj(r.get(32)?),
        consolidation: serde_json::from_str(&r.get::<_, String>(33)?).unwrap_or(Value::Null),
        tag_info: serde_json::from_str(&r.get::<_, String>(34)?).unwrap_or(Value::Null),
        centroid,
    })
}

/// What the import learned about a KM's origin and signature.
#[derive(Debug, Clone)]
pub struct Trust {
    pub source: Source,
    pub sig: SigState,
    pub overridden: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Owner {
    pub pinned: bool,
    pub banned: bool,
    pub note: Option<String>,
}

pub struct Store {
    pub root: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(home: &Path) -> Res<Store> {
        let root = home.join("km");
        for d in ["store", "tmp"] {
            std::fs::create_dir_all(root.join(d))
                .map_err(|e| Fail::new(500, "io", format!("{}: {e}", root.join(d).display())))?;
        }
        let conn = Connection::open(root.join("index.db")).map_err(db)?;
        conn.busy_timeout(std::time::Duration::from_secs(10)).map_err(db)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(SCHEMA).map_err(db)?;
        let s = Store {
            root,
            conn: Mutex::new(conn),
        };
        s.sweep_tmp();
        Ok(s)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn dir_of(&self, key: &str) -> PathBuf {
        self.root.join("store").join(key)
    }

    /// Staging folders left by an interrupted import are ours to remove.
    fn sweep_tmp(&self) {
        for e in std::fs::read_dir(self.tmp_dir()).into_iter().flatten().flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }

    fn log(conn: &Connection, action: &str, key: &str, detail: &Value) {
        let _ = conn.execute(
            "INSERT INTO history(at, action, key, detail) VALUES (?1, ?2, ?3, ?4)",
            params![now_ms(), action, key, detail.to_string()],
        );
    }

    pub fn disk_bytes(&self) -> u64 {
        self.conn
            .lock()
            .query_row("SELECT COALESCE(SUM(bytes), 0) FROM km", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    // ---- install / remove --------------------------------------------------------------------------------------

    /// Copies a verified KM folder into a staging folder (so the original can change without effect), verifies the
    /// copy, and returns the staged folder.
    pub fn stage_dir(&self, src: &Path, m: &Manifest) -> Res<PathBuf> {
        pack::verify_dir(src, m)?;
        let stage = self.tmp_dir().join(format!("import-{}", uuid::Uuid::new_v4().simple()));
        let to = stage.join(m.key());
        let copy = || -> std::io::Result<()> {
            for name in m.allowed_files() {
                let from = src.join(&name);
                if from.is_file() {
                    let dest = to.join(&name);
                    if let Some(p) = dest.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    std::fs::copy(&from, &dest)?;
                }
            }
            Ok(())
        };
        if let Err(e) = copy() {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(Fail::new(500, "io", format!("copying {}: {e}", src.display())));
        }
        if let Err(e) = pack::verify_dir(&to, m) {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
        Ok(stage)
    }

    /// Moves a verified, staged folder `<stage>/<key>/` into the store and records it. Returns the record and whether
    /// this exact KM was already installed (then nothing changes). Two different contents never share an
    /// `id@version`.
    pub fn install(&self, stage: &Path, m: &Manifest, trust: &Trust, disk_budget: u64) -> Res<(Record, bool)> {
        let key = m.key();
        let from = stage.join(&key);
        let done = |s: &Store| {
            let _ = std::fs::remove_dir_all(stage);
            s.get(&key)
        };
        if let Some(have) = self.get(&key)? {
            let r = if have.content_id == m.content_id {
                Ok((have, true))
            } else {
                Err(Fail::new(
                    409,
                    "version_exists",
                    format!(
                        "{key} is already installed with different content ({}…); a published version never changes: \
                         make a new version",
                        &have.content_id[..12]
                    ),
                ))
            };
            let _ = std::fs::remove_dir_all(stage);
            return r;
        }
        if disk_budget > 0 && self.disk_bytes().saturating_add(m.total_bytes()) > disk_budget {
            let _ = std::fs::remove_dir_all(stage);
            return Err(Fail::new(
                507,
                "disk_budget",
                format!(
                    "installing {key} ({} bytes) would pass the disk budget of {disk_budget} bytes ({} in use)",
                    m.total_bytes(),
                    self.disk_bytes()
                ),
            ));
        }
        let centroid = match &m.routing.centroid {
            Some(p) => {
                let b = std::fs::read(from.join(p)).map_err(|e| Fail::invalid(format!("{p}: {e}")))?;
                if b.is_empty() || b.len() % 2 != 0 || b.len() > 1 << 17 {
                    let _ = std::fs::remove_dir_all(stage);
                    return Err(Fail::invalid(format!(
                        "{p} must be f16 values (2 bytes each, 1 to 65536 of them); it is {} bytes",
                        b.len()
                    )));
                }
                Some(b)
            }
            None => None,
        };
        let dest = self.dir_of(&key);
        if dest.exists() {
            // a folder with no index row: left by a crash between the move and the insert
            let _ = std::fs::remove_dir_all(&dest);
        }
        std::fs::rename(&from, &dest).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dest.display())))?;
        let inserted = self.insert(m, &centroid, trust);
        if let Err(e) = inserted {
            let _ = std::fs::remove_dir_all(&dest);
            let _ = std::fs::remove_dir_all(stage);
            return Err(e);
        }
        let rec = done(self)?;
        Ok((rec.ok_or_else(|| db("the KM vanished after install"))?, false))
    }

    fn insert(&self, m: &Manifest, centroid: &Option<Vec<u8>>, t: &Trust) -> Res<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db)?;
        let key = m.key();
        let manifest = serde_json::to_string(&m.raw).unwrap_or_default();
        let consolidation: serde_json::Map<String, Value> =
            m.consolidation.iter().map(|(k, c)| (k.clone(), c.to_json())).collect();
        let tag_info: serde_json::Map<String, Value> = m
            .tag_meta
            .iter()
            .map(|(t, x)| (t.clone(), json!({"origin": x.origin, "confidence": x.confidence})))
            .collect();
        tx.execute(
            &format!("INSERT INTO km({COLS}, centroid, manifest) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36,?37)"),
            params![
                key,
                m.id,
                m.version.to_string(),
                m.content_id,
                m.kind.as_str(),
                m.name,
                m.description,
                m.domain,
                m.uuid,
                m.parent,
                m.license,
                jl(&m.tags),
                m.compat.base,
                m.compat.base_version,
                m.compat.arch,
                m.compat.base_hash,
                jl(&m.input),
                jl(&m.output),
                m.routing.descriptor,
                jl(&m.routing.keywords),
                m.routing.embed_model,
                m.routing.prior,
                m.resources.ram_bytes.map(|v| v as i64),
                m.resources.vram_bytes.map(|v| v as i64),
                m.total_bytes() as i64,
                t.source.as_str(),
                t.sig.label(),
                t.sig.key(),
                t.overridden as i64,
                Option::<String>::None,
                now_ms(),
                jl(&m.requires),
                jl(&m.conflicts),
                serde_json::to_string(&consolidation).unwrap_or_default(),
                serde_json::to_string(&tag_info).unwrap_or_default(),
                centroid,
                manifest,
            ],
        )
        .map_err(db)?;
        for tag in &m.tags {
            let meta = m.tag_meta.get(tag);
            tx.execute(
                "INSERT OR IGNORE INTO tags(key, tag, origin, confidence) VALUES (?1, ?2, ?3, ?4)",
                params![
                    key,
                    tag,
                    meta.map(|x| x.origin.as_str()).unwrap_or("manual"),
                    meta.map(|x| x.confidence).unwrap_or(1.0)
                ],
            )
            .map_err(db)?;
        }
        tx.execute(
            "INSERT INTO fts(key, name, description, descriptor, keywords) VALUES (?1,?2,?3,?4,?5)",
            params![
                key,
                m.name,
                m.description,
                m.routing.descriptor,
                m.routing.keywords.join(" ")
            ],
        )
        .map_err(db)?;
        Self::log(
            &tx,
            "import",
            &key,
            &json!({"content_id": m.content_id, "source": t.source.as_str(), "signature": t.sig.label(),
                    "overridden": t.overridden}),
        );
        tx.commit().map_err(db)
    }

    /// Removes a version from the index and the disk, and any channel that pointed at it.
    pub fn remove(&self, key: &str) -> Res<()> {
        let rec = self
            .get(key)?
            .ok_or_else(|| Fail::not_found(format!("{key} is not installed")))?;
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction().map_err(db)?;
            tx.execute("DELETE FROM km WHERE key = ?1", params![key]).map_err(db)?;
            tx.execute("DELETE FROM tags WHERE key = ?1", params![key])
                .map_err(db)?;
            tx.execute("DELETE FROM fts WHERE key = ?1", params![key]).map_err(db)?;
            let moved = tx
                .execute(
                    "DELETE FROM channels WHERE id = ?1 AND version = ?2",
                    params![rec.id, rec.version],
                )
                .map_err(db)?;
            Self::log(&tx, "remove", key, &json!({"channels_dropped": moved}));
            tx.commit().map_err(db)?;
        }
        let dir = self.dir_of(key);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dir.display())))?;
        }
        Ok(())
    }

    // ---- reading -----------------------------------------------------------------------------------------------

    pub fn get(&self, key: &str) -> Res<Option<Record>> {
        let conn = self.conn.lock();
        conn.query_row(&format!("SELECT {COLS} FROM km WHERE key = ?1"), params![key], |r| {
            record_of(r, None)
        })
        .optional()
        .map_err(db)
    }

    /// Every installed version, newest version first within an id; with centroids when asked.
    pub fn all(&self, with_centroid: bool) -> Res<Vec<Record>> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare(&format!("SELECT {COLS}, centroid FROM km ORDER BY id"))
            .map_err(db)?;
        let rows = st
            .query_map([], |r| {
                let c: Option<Vec<u8>> = r.get(35)?;
                record_of(r, if with_centroid { c } else { None })
            })
            .map_err(db)?;
        let mut v: Vec<Record> = rows.collect::<Result<_, _>>().map_err(db)?;
        v.sort_by(|a, b| a.id.cmp(&b.id).then(b.version().cmp(&a.version())));
        Ok(v)
    }

    pub fn centroid(&self, key: &str) -> Option<Vec<u8>> {
        self.conn
            .lock()
            .query_row("SELECT centroid FROM km WHERE key = ?1", params![key], |r| {
                r.get::<_, Option<Vec<u8>>>(0)
            })
            .ok()
            .flatten()
    }

    pub fn manifest_json(&self, key: &str) -> Option<Value> {
        let s: Option<String> = self
            .conn
            .lock()
            .query_row("SELECT manifest FROM km WHERE key = ?1", params![key], |r| r.get(0))
            .ok();
        s.and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn versions(&self, id: &str) -> Res<Vec<Record>> {
        Ok(self.all(false)?.into_iter().filter(|r| r.id == id).collect())
    }

    /// The KMs that carry `tag`, with the tag's `(origin, confidence)` on each.
    pub fn keys_with_tag(&self, tag: &str) -> Res<HashMap<String, (String, f64)>> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare("SELECT key, origin, confidence FROM tags WHERE tag = ?1")
            .map_err(db)?;
        let rows = st
            .query_map(params![tag], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))
            .map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    /// BM25 of the query over name, description, descriptor and keywords, as a positive number (larger is better),
    /// for every KM that matches at least one query word.
    pub fn lexical(&self, text: &str) -> Res<HashMap<String, f64>> {
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{}\"", w.to_lowercase()))
            .collect();
        if words.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.conn.lock();
        let mut st = conn
            .prepare("SELECT key, bm25(fts, 0.0, 3.0, 1.0, 2.0, 2.0) FROM fts WHERE fts MATCH ?1")
            .map_err(db)?;
        let rows = st
            .query_map(params![words.join(" OR ")], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(db)?;
        let mut out = HashMap::new();
        for r in rows {
            let (k, b) = r.map_err(db)?;
            out.insert(k, -b);
        }
        Ok(out)
    }

    pub fn set_misdeclared(&self, key: &str, reason: Option<&str>) -> Res<()> {
        let conn = self.conn.lock();
        conn.execute("UPDATE km SET misdeclared = ?2 WHERE key = ?1", params![key, reason])
            .map_err(db)?;
        Self::log(&conn, "misdeclared", key, &json!({"reason": reason}));
        Ok(())
    }

    // ---- channels ----------------------------------------------------------------------------------------------

    /// Points `channel` of `id` at `version`: one transaction, so readers see the old version or the new one. With
    /// `expect`, the move only happens if the channel still points at that version (or at none, for ""). Returns the
    /// version it pointed at before.
    pub fn channel_set(&self, id: &str, channel: &str, version: &str, expect: Option<&str>) -> Res<Option<String>> {
        if channel.is_empty()
            || channel.len() > 48
            || !channel
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err(Fail::invalid(
                "a channel name is lowercase letters, digits, '-' and '_' (at most 48 characters)",
            ));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db)?;
        let key = format!("{id}@{version}");
        let exists: bool = tx
            .query_row("SELECT 1 FROM km WHERE key = ?1", params![key], |_| Ok(true))
            .optional()
            .map_err(db)?
            .unwrap_or(false);
        if !exists {
            return Err(Fail::not_found(format!(
                "{key} is not installed, so a channel cannot point at it"
            )));
        }
        let prev: Option<String> = tx
            .query_row(
                "SELECT version FROM channels WHERE id = ?1 AND channel = ?2",
                params![id, channel],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        if let Some(want) = expect {
            if prev.as_deref().unwrap_or("") != want {
                return Err(Fail::new(
                    409,
                    "channel_moved",
                    format!(
                        "{id} {channel} points at {} now, not {want:?}; nothing was changed",
                        prev.as_deref().unwrap_or("nothing")
                    ),
                ));
            }
        }
        tx.execute(
            "INSERT INTO channels(id, channel, version, moved_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id, channel) DO UPDATE SET version = excluded.version, moved_at = excluded.moved_at",
            params![id, channel, version, now_ms()],
        )
        .map_err(db)?;
        Self::log(
            &tx,
            "channel",
            &format!("{id}:{channel}"),
            &json!({"from": prev, "to": version}),
        );
        tx.commit().map_err(db)?;
        Ok(prev)
    }

    pub fn channel_get(&self, id: &str, channel: &str) -> Option<String> {
        self.conn
            .lock()
            .query_row(
                "SELECT version FROM channels WHERE id = ?1 AND channel = ?2",
                params![id, channel],
                |r| r.get(0),
            )
            .ok()
    }

    /// `(channel, version, moved_at)` of an id.
    pub fn channels(&self, id: &str) -> Vec<(String, String, i64)> {
        let conn = self.conn.lock();
        let Ok(mut st) = conn.prepare("SELECT channel, version, moved_at FROM channels WHERE id = ?1 ORDER BY channel")
        else {
            return vec![];
        };
        st.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The version a request for `id` gets: a given version, a given channel, else `stable`, else the newest.
    pub fn resolve(&self, id: &str, version: Option<&str>, channel: Option<&str>) -> Res<Record> {
        let key = match (version, channel) {
            (Some(v), _) => format!("{id}@{v}"),
            (None, Some(c)) => {
                let v = self
                    .channel_get(id, c)
                    .ok_or_else(|| Fail::not_found(format!("{id} has no channel {c:?}")))?;
                format!("{id}@{v}")
            }
            (None, None) => match self.channel_get(id, "stable") {
                Some(v) => format!("{id}@{v}"),
                None => self
                    .versions(id)?
                    .into_iter()
                    .next()
                    .map(|r| r.key)
                    .ok_or_else(|| Fail::not_found(format!("no KM {id} is installed (km.list shows them)")))?,
            },
        };
        self.get(&key)?
            .ok_or_else(|| Fail::not_found(format!("{key} is not installed")))
    }

    // ---- the owner's pins and bans ----------------------------------------------------------------------------

    pub fn owner(&self, id: &str) -> Owner {
        self.conn
            .lock()
            .query_row(
                "SELECT pinned, banned, note FROM owner WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Owner {
                        pinned: r.get::<_, i64>(0)? != 0,
                        banned: r.get::<_, i64>(1)? != 0,
                        note: r.get(2)?,
                    })
                },
            )
            .unwrap_or_default()
    }

    pub fn owner_all(&self) -> HashMap<String, Owner> {
        let conn = self.conn.lock();
        let Ok(mut st) = conn.prepare("SELECT id, pinned, banned, note FROM owner") else {
            return HashMap::new();
        };
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Owner {
                    pinned: r.get::<_, i64>(1)? != 0,
                    banned: r.get::<_, i64>(2)? != 0,
                    note: r.get(3)?,
                },
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    fn set_owner(&self, id: &str, pinned: Option<bool>, banned: Option<bool>, note: Option<&str>) -> Res<()> {
        let cur = self.owner(id);
        let (p, b) = (pinned.unwrap_or(cur.pinned), banned.unwrap_or(cur.banned));
        let note = note.map(str::to_string).or(cur.note);
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO owner(id, pinned, banned, note, at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET pinned = ?2, banned = ?3, note = ?4, at = ?5",
            params![id, p as i64, b as i64, note, now_ms()],
        )
        .map_err(db)?;
        Self::log(&conn, "owner", id, &json!({"pinned": p, "banned": b, "note": note}));
        Ok(())
    }

    pub fn set_pinned(&self, id: &str, pinned: bool, note: Option<&str>) -> Res<()> {
        self.set_owner(id, Some(pinned), None, note)
    }

    pub fn set_banned(&self, id: &str, banned: bool, note: Option<&str>) -> Res<()> {
        self.set_owner(id, None, Some(banned), note)
    }

    // ---- history -----------------------------------------------------------------------------------------------

    /// The newest `limit` history rows, optionally for one key (an `id@version` or `id:channel`) or one action.
    pub fn history(&self, limit: usize, key: Option<&str>, action: Option<&str>) -> Vec<Value> {
        let conn = self.conn.lock();
        let Ok(mut st) = conn.prepare(
            "SELECT seq, at, action, key, detail FROM history
             WHERE (?1 IS NULL OR key = ?1 OR key LIKE ?1 || '@%' OR key LIKE ?1 || ':%') AND (?2 IS NULL OR action = ?2)
             ORDER BY seq DESC LIMIT ?3",
        ) else {
            return vec![];
        };
        st.query_map(params![key, action, limit as i64], |r| {
            let detail: Option<String> = r.get(4)?;
            Ok(
                json!({"seq": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "action": r.get::<_, String>(2)?,
                      "key": r.get::<_, String>(3)?,
                      "detail": detail.and_then(|d| serde_json::from_str::<Value>(&d).ok()).unwrap_or(Value::Null)}),
            )
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }
}

/// A bare record for tests elsewhere.
#[cfg(test)]
pub fn tests_record() -> Record {
    Record {
        key: "t@1.0.0".into(),
        id: "t".into(),
        version: "1.0.0".into(),
        content_id: String::new(),
        kind: "adapter".into(),
        name: "t".into(),
        description: String::new(),
        domain: String::new(),
        uuid: None,
        parent: None,
        license: None,
        tags: vec![],
        base: "dusty".into(),
        base_version: None,
        arch: None,
        base_hash: None,
        input: vec![],
        output: vec![],
        descriptor: String::new(),
        keywords: vec![],
        embed_model: None,
        prior: 0.0,
        ram_bytes: None,
        vram_bytes: None,
        bytes: 0,
        source: "own".into(),
        signature: "unsigned".into(),
        signed_by: None,
        overridden: false,
        misdeclared: None,
        installed_at: 0,
        requires: vec![],
        conflicts: vec![],
        consolidation: Value::Null,
        tag_info: Value::Null,
        centroid: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::sample;

    fn setup() -> (PathBuf, Store) {
        let home = crate::testing::temp_home();
        let s = Store::open(&home).unwrap();
        (home, s)
    }

    fn own() -> Trust {
        Trust {
            source: Source::Own,
            sig: SigState::Unsigned,
            overridden: false,
        }
    }

    fn add(s: &Store, home: &Path, id: &str, version: &str, descriptor: &str) -> Record {
        let dir = sample::adapter(&home.join("src"), id, version, descriptor);
        add_dir(s, &dir)
    }

    fn add_dir(s: &Store, dir: &Path) -> Record {
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(dir, &m).unwrap();
        s.install(&stage, &m, &own(), 0).unwrap().0
    }

    #[test]
    fn import_indexes_a_km_and_leaves_no_staging() {
        let (home, s) = setup();
        let r = add(&s, &home, "code.rust", "1.0.0", "rust async tokio");
        assert_eq!(r.key, "code.rust@1.0.0");
        assert_eq!(r.kind, "adapter");
        assert!(r.bytes > 2000);
        assert_eq!(r.embed_model.as_deref(), Some("hash-384"));
        assert!(s.centroid(&r.key).is_some_and(|c| c.len() == 384 * 2));
        assert!(s.dir_of(&r.key).join("km.toml").is_file());
        assert_eq!(std::fs::read_dir(s.tmp_dir()).unwrap().count(), 0);
        assert_eq!(s.keys_with_tag("lang:rust").unwrap().len(), 1);
        assert!(s.keys_with_tag("nope").unwrap().is_empty());
        assert_eq!(s.disk_bytes(), r.bytes);
        assert_eq!(s.history(10, Some("code.rust@1.0.0"), None).len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_same_version_with_other_content_is_refused_and_the_same_content_is_idempotent() {
        let (home, s) = setup();
        let r = add(&s, &home, "code.rust", "1.0.0", "rust async tokio");
        // same content again
        let dir = home.join("src").join("code.rust@1.0.0");
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(&dir, &m).unwrap();
        let (again, already) = s.install(&stage, &m, &own(), 0).unwrap();
        assert!(already && again.content_id == r.content_id);
        // other content under the same id@version
        let other = sample::adapter(&home.join("other"), "code.rust", "1.0.0", "something else entirely");
        let m2 = Manifest::from_file(&other.join("km.toml")).unwrap();
        let stage = s.stage_dir(&other, &m2).unwrap();
        let e = s.install(&stage, &m2, &own(), 0).unwrap_err();
        assert_eq!(e.code, "version_exists", "{}", e.message);
        assert!(!stage.exists(), "staging is cleaned");
        // the stored one is unchanged
        assert_eq!(s.get("code.rust@1.0.0").unwrap().unwrap().content_id, r.content_id);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_disk_budget_is_enforced() {
        let (home, s) = setup();
        let dir = sample::adapter(&home.join("src"), "code.rust", "1.0.0", "x");
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(&dir, &m).unwrap();
        let e = s.install(&stage, &m, &own(), 100).unwrap_err();
        assert_eq!(e.code, "disk_budget");
        assert!(s.get("code.rust@1.0.0").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_clears_index_folder_and_channels() {
        let (home, s) = setup();
        add(&s, &home, "code.rust", "1.0.0", "a");
        add(&s, &home, "code.rust", "1.1.0", "b");
        s.channel_set("code.rust", "stable", "1.0.0", None).unwrap();
        s.channel_set("code.rust", "candidate", "1.1.0", None).unwrap();
        s.remove("code.rust@1.0.0").unwrap();
        assert!(s.get("code.rust@1.0.0").unwrap().is_none());
        assert!(!s.dir_of("code.rust@1.0.0").exists());
        assert_eq!(s.channel_get("code.rust", "stable"), None);
        assert_eq!(s.channel_get("code.rust", "candidate").as_deref(), Some("1.1.0"));
        assert!(s.lexical("rust").unwrap().keys().all(|k| k != "code.rust@1.0.0"));
        assert_eq!(s.remove("code.rust@1.0.0").unwrap_err().status, 404);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn channel_moves_are_atomic_and_checked() {
        let (home, s) = setup();
        add(&s, &home, "code.rust", "1.0.0", "a");
        add(&s, &home, "code.rust", "1.1.0", "b");
        assert_eq!(s.channel_set("code.rust", "stable", "1.0.0", Some("")).unwrap(), None);
        // a move to a version that is not installed changes nothing and logs nothing
        let before = s.history(100, None, Some("channel")).len();
        let e = s.channel_set("code.rust", "stable", "9.9.9", None).unwrap_err();
        assert_eq!(e.status, 404);
        assert_eq!(s.channel_get("code.rust", "stable").as_deref(), Some("1.0.0"));
        assert_eq!(s.history(100, None, Some("channel")).len(), before);
        // compare-and-swap: a stale expectation fails and changes nothing
        let e = s
            .channel_set("code.rust", "stable", "1.1.0", Some("0.9.0"))
            .unwrap_err();
        assert_eq!(e.code, "channel_moved");
        assert_eq!(s.channel_get("code.rust", "stable").as_deref(), Some("1.0.0"));
        // a correct one moves it, and reports where it was
        let prev = s.channel_set("code.rust", "stable", "1.1.0", Some("1.0.0")).unwrap();
        assert_eq!(prev.as_deref(), Some("1.0.0"));
        // rollback is moving it back
        s.channel_set("code.rust", "stable", "1.0.0", None).unwrap();
        let h = s.history(100, Some("code.rust:stable"), Some("channel"));
        assert_eq!(h.len(), 3, "{h:?}");
        assert_eq!(h[0]["detail"]["to"], "1.0.0");
        assert!(s.channel_set("code.rust", "Bad Name", "1.0.0", None).is_err());
        // many movers: the channel always ends up at one valid version, one row
        std::thread::scope(|sc| {
            for i in 0..8 {
                let s = &s;
                sc.spawn(move || {
                    for _ in 0..10 {
                        let v = if i % 2 == 0 { "1.0.0" } else { "1.1.0" };
                        s.channel_set("code.rust", "stable", v, None).unwrap();
                        let got = s.channel_get("code.rust", "stable").unwrap();
                        assert!(got == "1.0.0" || got == "1.1.0");
                    }
                });
            }
        });
        assert_eq!(s.channels("code.rust").iter().filter(|c| c.0 == "stable").count(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_prefers_version_then_channel_then_stable_then_newest() {
        let (home, s) = setup();
        add(&s, &home, "code.rust", "1.0.0", "a");
        add(&s, &home, "code.rust", "1.2.0", "b");
        add(&s, &home, "code.rust", "1.10.0", "c");
        assert_eq!(
            s.resolve("code.rust", None, None).unwrap().version,
            "1.10.0",
            "newest by semver, not by text"
        );
        s.channel_set("code.rust", "stable", "1.0.0", None).unwrap();
        s.channel_set("code.rust", "candidate", "1.2.0", None).unwrap();
        assert_eq!(s.resolve("code.rust", None, None).unwrap().version, "1.0.0");
        assert_eq!(
            s.resolve("code.rust", None, Some("candidate")).unwrap().version,
            "1.2.0"
        );
        assert_eq!(
            s.resolve("code.rust", Some("1.10.0"), Some("candidate"))
                .unwrap()
                .version,
            "1.10.0"
        );
        assert_eq!(s.resolve("code.rust", None, Some("nope")).unwrap_err().status, 404);
        assert_eq!(s.resolve("nope", None, None).unwrap_err().status, 404);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn lexical_search_ranks_by_bm25() {
        let (home, s) = setup();
        add(&s, &home, "a.async", "1.0.0", "async tokio futures cancellation");
        add(&s, &home, "b.sql", "1.0.0", "sql postgres joins indexes");
        let hits = s.lexical("tokio cancellation").unwrap();
        assert!(hits.contains_key("a.async@1.0.0") && !hits.contains_key("b.sql@1.0.0"));
        assert!(hits["a.async@1.0.0"] > 0.0);
        // punctuation and quotes in a query are harmless
        assert!(s.lexical("\"tokio\" OR (* NEAR").is_ok());
        assert!(s.lexical("   ").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn owner_flags_persist_across_reopen() {
        let (home, s) = setup();
        s.set_pinned("a", true, Some("daily")).unwrap();
        s.set_banned("b", true, None).unwrap();
        s.set_pinned("a", false, None).unwrap();
        drop(s);
        let s = Store::open(&home).unwrap();
        let a = s.owner("a");
        assert!(!a.pinned && !a.banned && a.note.as_deref() == Some("daily"));
        assert!(s.owner("b").banned);
        assert_eq!(s.owner_all().len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_interrupted_import_is_swept_on_open() {
        let (home, s) = setup();
        let junk = s.tmp_dir().join("import-dead");
        std::fs::create_dir_all(&junk).unwrap();
        drop(s);
        let s = Store::open(&home).unwrap();
        assert!(!junk.exists());
        drop(s);
        let _ = std::fs::remove_dir_all(&home);
    }
}

//! Misty's retrieval index: documents in, cited passages out. One SQLite file (`<home>/misty/rag.db`).
//!
//! Lexical search (BM25 through FTS5) always works. Dense vectors are optional: when an embedding model is in the
//! roster the async layer embeds the chunks and calls [`Rag::set_vectors`]; a search then fuses both rankings with
//! reciprocal rank fusion. Vectors are compared exactly (brute force), which is fine for the sizes a personal index
//! reaches and keeps the index one plain file.
//!
//! This module is policy-free: it indexes whatever it is given. What Misty may read from disk is decided in `toolbox`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const CHUNK_CHARS: usize = 900;
pub const OVERLAP_CHARS: usize = 150;
const MAX_FILE_BYTES: u64 = 1_000_000;

const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "rst", "adoc", "org", "log", "rs", "py", "js", "mjs", "ts", "tsx", "jsx", "vue", "json",
    "toml", "yaml", "yml", "ini", "cfg", "csv", "tsv", "c", "h", "cpp", "hpp", "cc", "go", "java", "kt", "kts", "cs",
    "rb", "php", "sh", "ps1", "bat", "sql", "html", "css", "xml", "tex",
];
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".idea",
    ".vscode",
    ".gradle",
    "out",
];
const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "are", "was", "were", "it", "this", "that",
    "with", "as", "at", "by", "be", "from", "what", "which", "who", "how", "do", "does", "did", "i", "we", "you", "my",
    "me", "about", "can", "when", "where", "why", "whom", "whose", "there", "here", "tell", "please", "give", "show",
    "find", "has", "have", "had",
];

pub struct Rag {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    pub chunk: i64,
    pub collection: String,
    pub source: String,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
    /// Reciprocal-rank-fusion score (higher is better; only comparable within one search).
    pub score: f32,
    /// Share of the query's content words found in this chunk, 0 to 1.
    pub coverage: f32,
    /// `bm25`, `vector` or `both`.
    pub via: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Ingested {
    pub source: String,
    /// `new`, `updated` or `unchanged`.
    pub status: &'static str,
    pub chunks: usize,
    /// Chunks that have no vector yet (all of them for a new source).
    pub new_chunks: Vec<i64>,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct DirReport {
    pub files_seen: usize,
    pub new: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub chunks: usize,
    pub new_chunks: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SourceInfo {
    pub collection: String,
    pub source: String,
    pub chunks: usize,
    pub bytes: u64,
    pub vectors: usize,
}

pub fn sha(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Words of a query that carry meaning: lowercase alphanumerics of two or more characters, without stop words.
pub fn terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.chars().count() >= 2 && !STOP.contains(w))
        .filter(|w| seen.insert(w.to_string()))
        .map(String::from)
        .collect()
}

/// Cuts text into overlapping chunks along line boundaries, preferring blank lines and headings. Each chunk carries
/// its 1-based first and last line.
pub fn chunk_text(text: &str, target: usize, overlap: usize) -> Vec<(u32, u32, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = vec![];
    let (mut i, n) = (0usize, lines.len());
    while i < n {
        let (mut j, mut size) = (i, 0usize);
        let mut last_break = None;
        while j < n {
            let len = lines[j].chars().count() + 1;
            if size + len > target && j > i {
                break;
            }
            size += len;
            if lines[j].trim().is_empty() || (j + 1 < n && lines[j + 1].starts_with('#')) {
                last_break = Some(j + 1);
            }
            j += 1;
        }
        // a break found in the second half of the chunk beats cutting mid-paragraph; a chunk that ends at a natural
        // break starts the next one there, with no overlap (the overlap would drag the old section's tail along)
        let mut natural = false;
        if j < n {
            if let Some(b) = last_break {
                if b > i && b * 2 >= i + j {
                    j = b;
                    natural = true;
                }
            }
        }
        // a single line longer than the target is cut by characters
        if j == i {
            j = i + 1;
        }
        let body = lines[i..j].join("\n");
        let body = body.trim();
        if !body.is_empty() {
            if body.chars().count() > target * 2 {
                let chars: Vec<char> = body.chars().collect();
                for piece in chars.chunks(target) {
                    out.push((i as u32 + 1, j as u32, piece.iter().collect::<String>()));
                }
            } else {
                out.push((i as u32 + 1, j as u32, body.to_string()));
            }
        }
        if j >= n {
            break;
        }
        // step back over lines worth about `overlap` characters, but always move forward
        let (mut back, mut carried) = (j, 0usize);
        while !natural && back > i + 1 && carried < overlap {
            back -= 1;
            carried += lines[back].chars().count() + 1;
        }
        i = if back > i { back } else { j };
    }
    out
}

fn blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn unblob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

impl Rag {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::init(conn)
    }

    pub fn memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS sources(
               id INTEGER PRIMARY KEY, collection TEXT NOT NULL, uri TEXT NOT NULL, hash TEXT NOT NULL,
               bytes INTEGER NOT NULL, added INTEGER NOT NULL, UNIQUE(collection, uri));
             CREATE TABLE IF NOT EXISTS chunks(
               id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
               ord INTEGER NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, text TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS chunks_source ON chunks(source_id);
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, content='chunks', content_rowid='id', tokenize='porter unicode61');
             CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
               INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text); END;
             CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
               INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES('delete', old.id, old.text); END;
             CREATE TABLE IF NOT EXISTS vectors(
               chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
               model TEXT NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Adds or refreshes one document. An unchanged document (same SHA-256) costs nothing.
    pub fn ingest_text(&self, collection: &str, uri: &str, text: &str) -> Result<Ingested, String> {
        let hash = sha(text);
        let mut conn = self.conn.lock();
        let existing: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, hash FROM sources WHERE collection = ?1 AND uri = ?2",
                params![collection, uri],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let status = match &existing {
            Some((_, h)) if *h == hash => {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM chunks WHERE source_id = ?1",
                        params![existing.as_ref().unwrap().0],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(Ingested {
                    source: uri.into(),
                    status: "unchanged",
                    chunks: n as usize,
                    new_chunks: vec![],
                });
            }
            Some(_) => "updated",
            None => "new",
        };
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if let Some((id, _)) = existing {
            tx.execute("DELETE FROM sources WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        tx.execute(
            "INSERT INTO sources(collection, uri, hash, bytes, added) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![collection, uri, hash, text.len() as i64, now],
        )
        .map_err(|e| e.to_string())?;
        let sid = tx.last_insert_rowid();
        let mut ids = vec![];
        for (ord, (a, b, body)) in chunk_text(text, CHUNK_CHARS, OVERLAP_CHARS).into_iter().enumerate() {
            tx.execute(
                "INSERT INTO chunks(source_id, ord, start_line, end_line, text) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![sid, ord as i64, a, b, body],
            )
            .map_err(|e| e.to_string())?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Ingested {
            source: uri.into(),
            status,
            chunks: ids.len(),
            new_chunks: ids,
        })
    }

    /// Ingests a file or a folder tree of text files. `uri` of each is its path.
    pub fn ingest_path(&self, collection: &str, path: &Path) -> Result<DirReport, String> {
        let mut report = DirReport::default();
        let mut files: Vec<PathBuf> = vec![];
        if path.is_file() {
            files.push(path.to_path_buf());
        } else {
            walk(path, &mut files, 0);
        }
        files.sort();
        for f in files {
            report.files_seen += 1;
            // keys, tokens and the hub's own control files never go into an index a model can search
            if super::toolbox::is_secret(&f) {
                report.skipped += 1;
                continue;
            }
            let meta = match std::fs::metadata(&f) {
                Ok(m) => m,
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            };
            if meta.len() > MAX_FILE_BYTES || (!path.is_file() && !text_file(&f)) {
                report.skipped += 1;
                continue;
            }
            let bytes = match std::fs::read(&f) {
                Ok(b) => b,
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            };
            if bytes.contains(&0) {
                report.skipped += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let r = self.ingest_text(collection, &f.to_string_lossy(), &text)?;
            match r.status {
                "new" => report.new += 1,
                "updated" => report.updated += 1,
                _ => report.unchanged += 1,
            }
            report.chunks += r.chunks;
            report.new_chunks.extend(r.new_chunks);
        }
        Ok(report)
    }

    pub fn remove_source(&self, collection: &str, uri: &str) -> Result<bool, String> {
        let n = self
            .conn
            .lock()
            .execute(
                "DELETE FROM sources WHERE collection = ?1 AND uri = ?2",
                params![collection, uri],
            )
            .map_err(|e| e.to_string())?;
        Ok(n > 0)
    }

    pub fn remove_collection(&self, collection: &str) -> Result<usize, String> {
        self.conn
            .lock()
            .execute("DELETE FROM sources WHERE collection = ?1", params![collection])
            .map_err(|e| e.to_string())
    }

    pub fn sources(&self, collection: Option<&str>) -> Result<Vec<SourceInfo>, String> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare(
                "SELECT s.collection, s.uri, s.bytes, COUNT(c.id), COUNT(v.chunk_id)
                 FROM sources s LEFT JOIN chunks c ON c.source_id = s.id LEFT JOIN vectors v ON v.chunk_id = c.id
                 WHERE (?1 IS NULL OR s.collection = ?1) GROUP BY s.id ORDER BY s.collection, s.uri",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![collection], |r| {
                Ok(SourceInfo {
                    collection: r.get(0)?,
                    source: r.get(1)?,
                    bytes: r.get::<_, i64>(2)? as u64,
                    chunks: r.get::<_, i64>(3)? as usize,
                    vectors: r.get::<_, i64>(4)? as usize,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn collections(&self) -> Result<BTreeMap<String, (usize, usize)>, String> {
        let mut m: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for s in self.sources(None)? {
            let e = m.entry(s.collection).or_default();
            e.0 += 1;
            e.1 += s.chunks;
        }
        Ok(m)
    }

    /// Chunks of a collection with no vector from `model`: the async layer embeds these.
    pub fn unembedded(&self, collection: &str, model: &str, limit: usize) -> Result<Vec<(i64, String)>, String> {
        let conn = self.conn.lock();
        let mut st = conn
            .prepare(
                "SELECT c.id, c.text FROM chunks c JOIN sources s ON s.id = c.source_id
                 LEFT JOIN vectors v ON v.chunk_id = c.id AND v.model = ?2
                 WHERE s.collection = ?1 AND v.chunk_id IS NULL ORDER BY c.id LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![collection, model, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn set_vectors(&self, model: &str, items: &[(i64, Vec<f32>)]) -> Result<(), String> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (id, v) in items {
            tx.execute(
                "INSERT OR REPLACE INTO vectors(chunk_id, model, dim, vec) VALUES (?1, ?2, ?3, ?4)",
                params![id, model, v.len() as i64, blob(v)],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    fn hit_of(conn: &Connection, id: i64, collection: &str) -> Result<Option<Hit>, String> {
        conn.query_row(
            "SELECT s.uri, c.start_line, c.end_line, c.text FROM chunks c JOIN sources s ON s.id = c.source_id
             WHERE c.id = ?1 AND s.collection = ?2",
            params![id, collection],
            |r| {
                Ok(Hit {
                    chunk: id,
                    collection: collection.to_string(),
                    source: r.get(0)?,
                    start_line: r.get::<_, i64>(1)? as u32,
                    end_line: r.get::<_, i64>(2)? as u32,
                    text: r.get(3)?,
                    score: 0.0,
                    coverage: 0.0,
                    via: "bm25",
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    /// The best `k` chunks for `query`. `qvec` is the query's embedding with the model that made it; vectors from
    /// another model or of another size are ignored (never mixed).
    pub fn search(
        &self,
        collection: &str,
        query: &str,
        k: usize,
        qvec: Option<(&str, &[f32])>,
    ) -> Result<Vec<Hit>, String> {
        let q = terms(query);
        let conn = self.conn.lock();
        // lexical ranking
        let mut lexical: Vec<i64> = vec![];
        if !q.is_empty() {
            let fts = q
                .iter()
                .map(|t| format!("\"{}\"", t.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut st = conn
                .prepare(
                    "SELECT c.id FROM chunks_fts f JOIN chunks c ON c.id = f.rowid JOIN sources s ON s.id = c.source_id
                     WHERE chunks_fts MATCH ?1 AND s.collection = ?2 ORDER BY bm25(chunks_fts) LIMIT 60",
                )
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map(params![fts, collection], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            for r in rows {
                lexical.push(r.map_err(|e| e.to_string())?);
            }
        }
        // dense ranking
        let mut dense: Vec<i64> = vec![];
        if let Some((model, qv)) = qvec {
            let mut st = conn
                .prepare(
                    "SELECT v.chunk_id, v.vec FROM vectors v JOIN chunks c ON c.id = v.chunk_id JOIN sources s ON s.id = c.source_id
                     WHERE s.collection = ?1 AND v.model = ?2 AND v.dim = ?3",
                )
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map(params![collection, model, qv.len() as i64], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| e.to_string())?;
            let mut scored: Vec<(i64, f32)> = vec![];
            for r in rows {
                let (id, b) = r.map_err(|e| e.to_string())?;
                scored.push((id, cosine(qv, &unblob(&b))));
            }
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            dense = scored
                .into_iter()
                .filter(|(_, s)| *s > 0.0)
                .take(60)
                .map(|(i, _)| i)
                .collect();
        }
        // reciprocal rank fusion
        let mut fused: HashMap<i64, (f32, bool, bool)> = HashMap::new();
        for (rank, id) in lexical.iter().enumerate() {
            let e = fused.entry(*id).or_default();
            e.0 += 1.0 / (60.0 + rank as f32 + 1.0);
            e.1 = true;
        }
        for (rank, id) in dense.iter().enumerate() {
            let e = fused.entry(*id).or_default();
            e.0 += 1.0 / (60.0 + rank as f32 + 1.0);
            e.2 = true;
        }
        let mut order: Vec<(i64, f32, bool, bool)> = fused.into_iter().map(|(i, (s, l, d))| (i, s, l, d)).collect();
        order.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let mut hits = vec![];
        for (id, score, l, d) in order.into_iter().take(k) {
            if let Some(mut h) = Self::hit_of(&conn, id, collection)? {
                h.score = score;
                h.via = match (l, d) {
                    (true, true) => "both",
                    (false, true) => "vector",
                    _ => "bm25",
                };
                let lower = h.text.to_lowercase();
                h.coverage = if q.is_empty() {
                    0.0
                } else {
                    q.iter().filter(|t| lower.contains(t.as_str())).count() as f32 / q.len() as f32
                };
                hits.push(h);
            }
        }
        Ok(hits)
    }

    /// How strongly the index answers `query`: the best chunk's coverage of the query's words (0 when nothing matches).
    pub fn strength(&self, collection: &str, query: &str) -> f32 {
        self.search(collection, query, 3, None)
            .ok()
            .and_then(|h| {
                h.iter()
                    .map(|x| x.coverage)
                    .fold(None, |m: Option<f32>, c| Some(m.map_or(c, |m| m.max(c))))
            })
            .unwrap_or(0.0)
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 12 || out.len() > 20_000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // a link could lead out of the folder the owner chose
        }
        if ft.is_dir() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&p, out, depth + 1);
            }
        } else if ft.is_file() {
            out.push(p);
        }
    }
}

/// Numbered passages for a prompt, within a token budget (about four characters a token), and the same list as
/// citations. A passage that does not fit whole is dropped, not cut: half a fact misleads.
pub fn pack(hits: &[Hit], budget_tokens: usize) -> (String, Vec<serde_json::Value>) {
    let mut text = String::new();
    let mut cites = vec![];
    let mut used = 0usize;
    let mut seen = HashSet::new();
    for h in hits {
        if !seen.insert(sha(&h.text)) {
            continue;
        }
        let block = format!(
            "[{}] {} (lines {}-{})\n{}\n\n",
            cites.len() + 1,
            h.source,
            h.start_line,
            h.end_line,
            h.text
        );
        let cost = block.chars().count() / 4 + 1;
        if used + cost > budget_tokens && !cites.is_empty() {
            break;
        }
        if used + cost > budget_tokens {
            continue;
        }
        used += cost;
        text.push_str(&block);
        cites.push(
            serde_json::json!({"n": cites.len() + 1, "source": h.source, "start_line": h.start_line,
                                      "end_line": h.end_line, "chunk": h.chunk, "via": h.via, "coverage": h.coverage}),
        );
    }
    (text, cites)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_drop_stop_words_and_duplicates() {
        assert_eq!(
            terms("What is the release date of the release?"),
            vec!["release", "date"]
        );
        assert!(terms("a an the").is_empty());
    }

    #[test]
    fn chunks_follow_line_boundaries_overlap_and_cover_everything() {
        let text: String = (1..=60)
            .map(|i| format!("line number {i} with some words in it\n"))
            .collect();
        let chunks = chunk_text(&text, 300, 80);
        assert!(chunks.len() > 3);
        assert_eq!(chunks[0].0, 1);
        assert_eq!(chunks.last().unwrap().1, 60);
        for pair in chunks.windows(2) {
            assert!(pair[1].0 <= pair[0].1 + 1, "no gap between chunks");
            assert!(pair[1].0 > pair[0].0, "always moves forward");
        }
        assert!(chunks.iter().all(|c| c.2.chars().count() <= 600));
    }

    #[test]
    fn a_very_long_line_is_cut_and_empty_text_makes_no_chunks() {
        let long = "x".repeat(5000);
        assert!(chunk_text(&long, 900, 100).len() >= 5);
        assert!(chunk_text("", 900, 100).is_empty());
        assert!(chunk_text("\n\n  \n", 900, 100).is_empty());
    }

    #[test]
    fn headings_are_preferred_break_points() {
        let mut text = String::new();
        for i in 0..8 {
            text.push_str(&format!("# Section {i}\n"));
            for j in 0..6 {
                text.push_str(&format!("section {i} sentence {j} filler filler filler filler\n"));
            }
            text.push('\n');
        }
        let chunks = chunk_text(&text, 500, 60);
        assert!(chunks.iter().skip(1).filter(|c| c.2.starts_with("# Section")).count() >= 2);
    }

    #[test]
    fn ingest_reports_new_unchanged_and_updated_and_search_finds_the_passage() {
        let rag = Rag::memory().unwrap();
        let a = rag
            .ingest_text(
                "default",
                "notes/a.md",
                "# Release\nThe release date is the 14th of March.\nUnrelated gardening text.",
            )
            .unwrap();
        assert_eq!((a.status, a.chunks > 0), ("new", true));
        let again = rag
            .ingest_text(
                "default",
                "notes/a.md",
                "# Release\nThe release date is the 14th of March.\nUnrelated gardening text.",
            )
            .unwrap();
        assert_eq!(again.status, "unchanged");
        assert!(again.new_chunks.is_empty());
        rag.ingest_text("default", "notes/b.md", "Soup recipes: carrot, onion and thyme.")
            .unwrap();
        let hits = rag.search("default", "when is the release date", 3, None).unwrap();
        assert_eq!(hits[0].source, "notes/a.md");
        assert!(hits[0].coverage >= 0.9, "{:?}", hits[0].coverage);
        let upd = rag
            .ingest_text("default", "notes/a.md", "The release moved to April.")
            .unwrap();
        assert_eq!(upd.status, "updated");
        let hits = rag.search("default", "release April", 3, None).unwrap();
        assert!(hits[0].text.contains("April"));
        assert!(
            rag.search("default", "March", 3, None).unwrap().is_empty(),
            "the old text is gone from the index"
        );
    }

    #[test]
    fn collections_are_separate_and_removal_cascades() {
        let rag = Rag::memory().unwrap();
        rag.ingest_text("a", "x", "alpha beta gamma").unwrap();
        rag.ingest_text("b", "x", "alpha delta").unwrap();
        assert_eq!(rag.search("a", "gamma", 5, None).unwrap().len(), 1);
        assert!(rag.search("b", "gamma", 5, None).unwrap().is_empty());
        assert_eq!(rag.collections().unwrap().len(), 2);
        assert!(rag.remove_source("a", "x").unwrap());
        assert!(rag.search("a", "alpha", 5, None).unwrap().is_empty());
        assert_eq!(rag.remove_collection("b").unwrap(), 1);
        assert!(rag.sources(None).unwrap().is_empty());
    }

    #[test]
    fn vectors_fuse_with_the_lexical_ranking_and_other_models_are_never_mixed() {
        let rag = Rag::memory().unwrap();
        let a = rag.ingest_text("c", "cats", "felines purr softly").unwrap();
        let b = rag.ingest_text("c", "dogs", "canines bark loudly").unwrap();
        // the query shares no word with either chunk: only the vectors can find it
        rag.set_vectors(
            "m1",
            &[(a.new_chunks[0], vec![1.0, 0.0]), (b.new_chunks[0], vec![0.0, 1.0])],
        )
        .unwrap();
        assert!(rag.search("c", "kitten", 2, None).unwrap().is_empty());
        let hits = rag.search("c", "kitten", 2, Some(("m1", &[0.9, 0.1]))).unwrap();
        assert_eq!(hits[0].source, "cats");
        assert_eq!(hits[0].via, "vector");
        assert!(rag
            .search("c", "kitten", 2, Some(("other-model", &[0.9, 0.1])))
            .unwrap()
            .is_empty());
        assert!(
            rag.search("c", "kitten", 2, Some(("m1", &[0.9, 0.1, 0.0])))
                .unwrap()
                .is_empty(),
            "wrong size is ignored"
        );
        let both = rag.search("c", "felines", 2, Some(("m1", &[1.0, 0.0]))).unwrap();
        assert_eq!(both[0].via, "both");
        assert_eq!(rag.unembedded("c", "m1", 10).unwrap().len(), 0);
        assert_eq!(rag.unembedded("c", "m2", 10).unwrap().len(), 2);
    }

    #[test]
    fn folders_are_walked_with_the_right_files_and_changes_are_incremental() {
        let d = crate::testing::TestDir::new("misty-rag");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join("README.md"), "# Project\nA tool for indexing.").unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() { println!(\"indexing\"); }").unwrap();
        std::fs::write(d.join("node_modules/x/skip.js"), "ignored").unwrap();
        std::fs::write(d.join(".git/config"), "ignored").unwrap();
        std::fs::write(d.join("image.png"), [0u8, 1, 2, 0]).unwrap();
        std::fs::write(d.join("data.bin"), [0u8, 159, 146, 150]).unwrap();
        let rag = Rag::memory().unwrap();
        let r = rag.ingest_path("p", &d).unwrap();
        assert_eq!((r.new, r.unchanged, r.updated), (2, 0, 0), "{r:?}");
        assert!(r.skipped >= 2);
        let r = rag.ingest_path("p", &d).unwrap();
        assert_eq!((r.new, r.unchanged), (0, 2));
        std::fs::write(d.join("README.md"), "# Project\nChanged text about indexing.").unwrap();
        let r = rag.ingest_path("p", &d).unwrap();
        assert_eq!((r.updated, r.unchanged), (1, 1));
        let hits = rag.search("p", "println indexing", 5, None).unwrap();
        assert!(hits.iter().any(|h| h.source.ends_with("main.rs")));
        assert!(!rag
            .sources(Some("p"))
            .unwrap()
            .iter()
            .any(|s| s.source.contains("node_modules")));
    }

    #[test]
    fn packing_respects_the_budget_dedups_and_numbers_the_sources() {
        let mk = |n: i64, text: &str| Hit {
            chunk: n,
            collection: "c".into(),
            source: format!("s{n}"),
            start_line: 1,
            end_line: 2,
            text: text.into(),
            score: 1.0,
            coverage: 1.0,
            via: "bm25",
        };
        let hits = vec![
            mk(1, &"a ".repeat(100)),
            mk(2, &"a ".repeat(100)),
            mk(3, &"b ".repeat(100)),
            mk(4, &"c ".repeat(4000)),
        ];
        let (text, cites) = pack(&hits, 120);
        assert_eq!(
            cites.len(),
            2,
            "the duplicate and the oversize passage are dropped: {text}"
        );
        assert!(text.starts_with("[1] s1") && text.contains("[2] s3"));
        assert_eq!(cites[1]["n"], 2);
        let (_, none) = pack(&hits[3..], 100);
        assert!(
            none.is_empty(),
            "one passage that cannot fit gives nothing, not half a passage"
        );
    }
}

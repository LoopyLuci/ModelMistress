//! Gathering: sources become chunks that keep their origin (file and lines, URL, KM, trace, licence when known).
//! Everything read here is data. Fetched or read text is never treated as instructions and never executed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::checks::ToolDef;
use super::store::ChunkRow;
use crate::engine::{Engine, Fail};
use crate::km::{manifest::Manifest, Km};

type Res<T> = Result<T, Fail>;

#[derive(Debug, Clone)]
pub struct Limits {
    pub chunk_chars: usize,
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_url_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            chunk_chars: 1200,
            max_file_bytes: 512 * 1024,
            max_files: 500,
            max_total_bytes: 16 * 1024 * 1024,
            max_url_bytes: 1024 * 1024,
        }
    }
}

#[derive(Default)]
pub struct Gathered {
    pub chunks: Vec<ChunkRow>,
    pub tools: Vec<ToolDef>,
    /// What was skipped or truncated, and why.
    pub notes: Vec<String>,
    pub files: usize,
    pub bytes: u64,
}

const TEXT_EXT: [&str; 28] = [
    "md", "markdown", "txt", "rst", "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp", "cs",
    "toml", "yaml", "yml", "json", "sh", "sql", "html", "css", "vue", "kt", "swift",
];
const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", "__pycache__", ".venv", "dist"];

// ---- globs -------------------------------------------------------------------------------------------------------

fn wild(p: &[char], s: &[char]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some('*'), _) => wild(&p[1..], s) || (!s.is_empty() && wild(p, &s[1..])),
        (Some('?'), Some(_)) => wild(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a.to_lowercase().eq(b.to_lowercase()) => wild(&p[1..], &s[1..]),
        _ => false,
    }
}

fn segs(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => segs(&p[1..], s) || (!s.is_empty() && segs(p, &s[1..])),
        Some(seg) => {
            !s.is_empty() && {
                let (a, b): (Vec<char>, Vec<char>) = (seg.chars().collect(), s[0].chars().collect());
                wild(&a, &b) && segs(&p[1..], &s[1..])
            }
        }
    }
}

/// `**` crosses folders, `*` and `?` do not; case-insensitive; `/` separates (a pattern without `/` matches names at
/// any depth).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let s: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
    let pattern = pattern.replace('\\', "/");
    let full = if pattern.contains('/') {
        pattern
    } else {
        format!("**/{pattern}")
    };
    let p: Vec<&str> = full.split('/').filter(|x| !x.is_empty()).collect();
    segs(&p, &s)
}

// ---- chunking ----------------------------------------------------------------------------------------------------

/// Splits text into pieces of about `max` characters at line breaks (blank lines preferred); returns
/// `(text, first line, last line)`, lines counted from 1.
pub fn chunk_text(text: &str, max: usize) -> Vec<(String, usize, usize)> {
    // a line longer than a chunk is cut
    let mut lines: Vec<(usize, String)> = vec![];
    for (i, l) in text.lines().enumerate() {
        if l.chars().count() > max {
            let cs: Vec<char> = l.chars().collect();
            lines.extend(cs.chunks(max).map(|p| (i + 1, p.iter().collect::<String>())));
        } else {
            lines.push((i + 1, l.to_string()));
        }
    }
    let size_of = |ls: &[(usize, String)]| ls.iter().map(|l| l.1.chars().count() + 1).sum::<usize>();
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        let mut size = 0;
        while j < lines.len() {
            let add = lines[j].1.chars().count() + 1;
            if size + add > max && j > i {
                break;
            }
            size += add;
            j += 1;
        }
        let mut end = j;
        if j < lines.len() {
            // break after the last blank line if that leaves the piece at least half full
            if let Some(b) = (i..j).rev().find(|&k| lines[k].1.trim().is_empty()) {
                if b > i && size_of(&lines[i..b]) * 2 >= max {
                    end = b + 1;
                }
            }
        }
        let body = lines[i..end].iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join("
");
        if !body.trim().is_empty() {
            out.push((body.trim_end().to_string(), lines[i].0, lines[end - 1].0));
        }
        i = end;
    }
    out
}

// ---- licences ----------------------------------------------------------------------------------------------------

fn spdx_of(text: &str) -> Option<String> {
    let head: String = text.chars().take(2048).collect();
    let i = head.find("SPDX-License-Identifier:")?;
    let rest = &head[i + "SPDX-License-Identifier:".len()..];
    let id = rest.lines().next()?.trim().trim_end_matches("*/").trim().to_string();
    (!id.is_empty() && id.len() < 64).then_some(id)
}

fn license_file(dir: &Path) -> Option<String> {
    for n in ["LICENSE", "LICENSE.md", "LICENSE.txt", "COPYING"] {
        let Ok(t) = std::fs::read_to_string(dir.join(n)) else { continue };
        let head: String = t.chars().take(600).collect::<String>().to_lowercase();
        let id = if head.contains("apache license") {
            "Apache-2.0"
        } else if head.contains("mit license") || head.contains("permission is hereby granted, free of charge") {
            "MIT"
        } else if head.contains("gnu affero") {
            "AGPL-3.0"
        } else if head.contains("gnu general public") {
            "GPL-3.0"
        } else if head.contains("bsd") {
            "BSD-3-Clause"
        } else if head.contains("mozilla public") {
            "MPL-2.0"
        } else {
            continue;
        };
        return Some(id.into());
    }
    None
}

// ---- reading sources ---------------------------------------------------------------------------------------------

struct Sink<'a> {
    out: &'a mut Gathered,
    limits: &'a Limits,
}

impl Sink<'_> {
    fn add_text(&mut self, text: &str, role: &str, origin: Value) {
        for (body, a, b) in chunk_text(text, self.limits.chunk_chars) {
            let mut o = origin.clone();
            o["line_start"] = json!(a);
            o["line_end"] = json!(b);
            let ord = self.out.chunks.len() as i64;
            self.out.chunks.push(ChunkRow {
                id: format!("c{}", ord + 1),
                ord,
                text: body,
                origin: o,
                role: role.into(),
                emb: None,
            });
        }
    }
}

fn read_text_file(path: &Path, limits: &Limits, notes: &mut Vec<String>, label: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > limits.max_file_bytes {
        notes.push(format!("{label}: skipped, {} bytes is over the {} byte limit", meta.len(), limits.max_file_bytes));
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        notes.push(format!("{label}: skipped, looks binary"));
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(PathBuf, String)>, cap: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= cap * 4 {
            return; // enough candidates; the file limit decides
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // links are never followed
        }
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&e.path(), &r, out, cap);
            }
        } else if ft.is_file() {
            out.push((e.path(), r));
        }
    }
}

fn strs(v: &Value, k: &str) -> Vec<String> {
    v[k].as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn gather_dir(src: &Value, limits: &Limits, default_license: Option<&str>, sink: &mut Sink) -> Res<()> {
    let root = PathBuf::from(src["path"].as_str().unwrap_or(""));
    if !root.is_dir() {
        return Err(Fail::invalid(format!("source dir {}: not a folder", root.display())));
    }
    let globs = strs(src, "globs");
    let exclude = strs(src, "exclude");
    let max_files = src["max_files"].as_u64().map(|n| n as usize).unwrap_or(limits.max_files);
    let lic_dir = license_file(&root).or_else(|| default_license.map(str::to_string));
    let mut found = vec![];
    walk(&root, "", &mut found, max_files);
    let base = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut taken = 0usize;
    for (path, rel) in found {
        let wanted = if globs.is_empty() {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| TEXT_EXT.contains(&e.to_lowercase().as_str()))
        } else {
            globs.iter().any(|g| glob_match(g, &rel))
        };
        if !wanted || exclude.iter().any(|g| glob_match(g, &rel)) {
            continue;
        }
        if taken >= max_files {
            sink.out.notes.push(format!("{base}: stopped at {max_files} files (size limit)"));
            break;
        }
        if sink.out.bytes >= limits.max_total_bytes {
            sink.out.notes.push(format!("{base}: stopped at {} bytes in total", limits.max_total_bytes));
            break;
        }
        let Some(text) = read_text_file(&path, limits, &mut sink.out.notes, &format!("{base}/{rel}")) else {
            continue;
        };
        taken += 1;
        sink.out.files += 1;
        sink.out.bytes += text.len() as u64;
        let lic = spdx_of(&text).or_else(|| lic_dir.clone());
        sink.add_text(&text, "doc", json!({"kind": "file", "path": format!("{base}/{rel}"), "license": lic}));
    }
    Ok(())
}

fn gather_file(src: &Value, limits: &Limits, default_license: Option<&str>, sink: &mut Sink) -> Res<()> {
    let path = PathBuf::from(src["path"].as_str().unwrap_or(""));
    if !path.is_file() {
        return Err(Fail::invalid(format!("source file {}: not a file", path.display())));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(text) = read_text_file(&path, limits, &mut sink.out.notes, &name) else {
        return Err(Fail::invalid(format!("source file {name}: too large or not text")));
    };
    sink.out.files += 1;
    sink.out.bytes += text.len() as u64;
    let lic = spdx_of(&text).or_else(|| default_license.map(str::to_string));
    sink.add_text(&text, "doc", json!({"kind": "file", "path": name, "license": lic}));
    Ok(())
}

fn gather_skill(src: &Value, limits: &Limits, sink: &mut Sink) -> Res<()> {
    let root = PathBuf::from(src["path"].as_str().unwrap_or(""));
    if !root.is_dir() {
        return Err(Fail::invalid(format!("skill folder {}: not a folder", root.display())));
    }
    let skill_md = root.join("SKILL.md");
    if !skill_md.is_file() {
        return Err(Fail::invalid(format!("skill folder {} has no SKILL.md", root.display())));
    }
    let base = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut found = vec![];
    walk(&root, "", &mut found, limits.max_files);
    let lic = license_file(&root);
    for (path, rel) in found {
        let is_skill = rel == "SKILL.md";
        let text_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| TEXT_EXT.contains(&e.to_lowercase().as_str()));
        if !is_skill && !text_ext {
            continue;
        }
        let Some(text) = read_text_file(&path, limits, &mut sink.out.notes, &format!("{base}/{rel}")) else {
            continue;
        };
        sink.out.files += 1;
        sink.out.bytes += text.len() as u64;
        sink.add_text(
            &text,
            if is_skill { "skill" } else { "doc" },
            json!({"kind": "skill", "path": format!("{base}/{rel}"), "license": lic}),
        );
    }
    Ok(())
}

fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style"] {
        loop {
            let lower = s.to_lowercase();
            let Some(a) = lower.find(&format!("<{tag}")) else { break };
            let end = lower[a..]
                .find(&format!("</{tag}>"))
                .map(|e| a + e + tag.len() + 3)
                .unwrap_or(s.len());
            s.replace_range(a..end.min(s.len()), " ");
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn gather_url(
    engine: &Engine,
    src: &Value,
    limits: &Limits,
    default_license: Option<&str>,
    sink: &mut Sink<'_>,
) -> Res<()> {
    let url = src["url"].as_str().unwrap_or("");
    let cap = src["max_bytes"].as_u64().unwrap_or(limits.max_url_bytes).min(limits.max_url_bytes * 8);
    let mut resp = engine
        .http
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .header("user-agent", "model-mistress-factory")
        .send()
        .await
        .map_err(|e| Fail::new(502, "fetch_failed", format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Fail::new(502, "fetch_failed", format!("{url}: HTTP {}", resp.status())));
    }
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !(ctype.is_empty() || ctype.starts_with("text/") || ctype.contains("json") || ctype.contains("xml")) {
        return Err(Fail::invalid(format!("{url}: content type {ctype:?} is not text")));
    }
    let mut body: Vec<u8> = vec![];
    let mut truncated = false;
    while let Some(part) = resp
        .chunk()
        .await
        .map_err(|e| Fail::new(502, "fetch_failed", format!("{url}: {e}")))?
    {
        body.extend_from_slice(&part);
        if body.len() as u64 > cap {
            body.truncate(cap as usize);
            truncated = true;
            break;
        }
    }
    if truncated {
        sink.out.notes.push(format!("{url}: cut at {cap} bytes (size cap)"));
    }
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if ctype.contains("html") || text.trim_start().to_lowercase().starts_with("<!doctype html") {
        text = html_to_text(&text);
    }
    sink.out.files += 1;
    sink.out.bytes += text.len() as u64;
    let lic = spdx_of(&text).or_else(|| default_license.map(str::to_string));
    sink.add_text(&text, "doc", json!({"kind": "url", "url": url, "license": lic}));
    Ok(())
}

fn gather_km(km: &Km, src: &Value, sink: &mut Sink) -> Res<()> {
    let (id, ver, chan) = crate::km::split_ref(src["id"].as_str().unwrap_or(""));
    let rec = km.store.resolve(&id, ver.as_deref(), chan.as_deref())?;
    let dir = km.store.dir_of(&rec.key);
    let m = Manifest::from_file(&dir.join("km.toml"))?;
    let lic = m.license.clone();
    if let Some(mem) = &m.memory {
        let text = std::fs::read_to_string(dir.join(&mem.chunks))
            .map_err(|e| Fail::invalid(format!("{}: {e}", rec.key)))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
            let Some(t) = v["text"].as_str() else { continue };
            let ord = sink.out.chunks.len() as i64;
            sink.out.chunks.push(ChunkRow {
                id: format!("c{}", ord + 1),
                ord,
                text: t.to_string(),
                origin: json!({"kind": "km", "km": rec.key, "chunk": v["id"], "source": v["source"], "license": lic}),
                role: "doc".into(),
                emb: None,
            });
        }
    } else if let Some(p) = m.payload_of("records") {
        let text = std::fs::read_to_string(dir.join(&p.path))
            .map_err(|e| Fail::invalid(format!("{}: {e}", rec.key)))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
            if v["status"] == "rejected" {
                continue;
            }
            let q = v["input"]
                .as_array()
                .and_then(|a| a.iter().rev().find(|m| m["role"] == "user"))
                .and_then(|m| m["content"].as_str())
                .unwrap_or("");
            let a = v["output"].as_str().unwrap_or("");
            if q.is_empty() || a.is_empty() {
                continue;
            }
            let ord = sink.out.chunks.len() as i64;
            sink.out.chunks.push(ChunkRow {
                id: format!("c{}", ord + 1),
                ord,
                text: format!("Q: {q}\nA: {a}"),
                origin: json!({"kind": "km", "km": rec.key, "record": v["id"], "license": lic}),
                role: "doc".into(),
                emb: None,
            });
        }
    } else if let Some(sk) = &m.skill {
        let text = std::fs::read_to_string(dir.join(&sk.instructions))
            .map_err(|e| Fail::invalid(format!("{}: {e}", rec.key)))?;
        sink.add_text(&text, "doc", json!({"kind": "km", "km": rec.key, "license": lic}));
    } else {
        return Err(Fail::invalid(format!(
            "{} is a {} KM; only memory, data and skill KMs can be a source",
            rec.key,
            rec.kind
        )));
    }
    Ok(())
}

/// A tool from a hub operation (`name` only) or given inline (an MCP tool's schema).
pub fn tool_def(src: &Value) -> Res<ToolDef> {
    let name = src["name"].as_str().unwrap_or("").to_string();
    if let Some(schema) = src.get("input_schema").filter(|s| s.is_object()) {
        let provider = src["provider"].as_str().unwrap_or("hub");
        if provider != "hub" && !provider.starts_with("mcp:") {
            return Err(Fail::invalid(format!("tool {name}: provider must be hub or mcp:<server>")));
        }
        return Ok(ToolDef {
            name,
            provider: provider.into(),
            description: src["description"].as_str().unwrap_or("").to_string(),
            input_schema: schema.clone(),
            side_effects: src["side_effects"].as_str().unwrap_or("none").to_string(),
        });
    }
    let ops = crate::hub::operations();
    let Some(op) = ops.iter().find(|o| o["id"] == name.as_str()) else {
        return Err(Fail::invalid(format!(
            "tool {name:?} is not a hub operation; give provider (mcp:<server>), description and input_schema for an MCP tool"
        )));
    };
    let side = if op["destructive"].as_bool().unwrap_or(false) || op["mutating"].as_bool().unwrap_or(false) {
        "write"
    } else {
        "read"
    };
    Ok(ToolDef {
        name,
        provider: "hub".into(),
        description: op["summary"].as_str().unwrap_or("").to_string(),
        input_schema: op["input_schema"].clone(),
        side_effects: src["side_effects"].as_str().unwrap_or(side).to_string(),
    })
}

pub fn tool_text(t: &ToolDef) -> String {
    format!(
        "Tool: {}\nProvider: {}\nSide effects: {}\nDescription: {}\nArguments (JSON schema): {}",
        t.name,
        t.provider,
        t.side_effects,
        t.description,
        serde_json::to_string(&t.input_schema).unwrap_or_default()
    )
}

fn gather_traces(home: &Path, src: &Value, sink: &mut Sink) -> Res<()> {
    let tr = crate::trace::tracer(home);
    let store = tr.store().map_err(Fail::unavailable)?;
    let n = src["limit"].as_u64().unwrap_or(30).min(200);
    let (_, items) = store
        .list(&crate::trace::store::Filter::from_json(
            &json!({"q": src["query"], "status": "ok", "limit": n * 2}),
        ))
        .map_err(Fail::unavailable)?;
    let mut taken = 0;
    for it in items {
        // the factory's own model calls are not material for itself
        if !it["task_id"].is_null() || taken >= n {
            continue;
        }
        let id = it["id"].as_str().unwrap_or("");
        let Ok(Some(full)) = store.get(id, 0) else { continue };
        let (p, a) = (full["prompt"].as_str().unwrap_or(""), full["answer"].as_str().unwrap_or(""));
        if p.is_empty() || a.is_empty() {
            continue;
        }
        taken += 1;
        sink.out.files += 1;
        sink.add_text(&format!("User: {p}\nAssistant: {a}"), "doc", json!({"kind": "trace", "trace": id}));
    }
    if taken == 0 {
        sink.out.notes.push(format!("traces {:?}: no matching traces", src["query"]));
    }
    Ok(())
}

/// A short look at the sources for the planner: file names and the start of the first file. Reads little, fetches
/// nothing.
pub fn preview(sources: &[Value]) -> String {
    let mut out = String::new();
    let head = |p: &Path, n: usize| -> String {
        std::fs::read(p)
            .ok()
            .filter(|b| !b.iter().take(4096).any(|x| *x == 0))
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(n * 4)]).chars().take(n).collect())
            .unwrap_or_default()
    };
    for s in sources.iter().take(6) {
        match s["kind"].as_str().unwrap_or("") {
            "dir" => {
                let root = PathBuf::from(s["path"].as_str().unwrap_or(""));
                let globs = strs(s, "globs");
                let mut found = vec![];
                walk(&root, "", &mut found, 50);
                found.retain(|(p, rel)| {
                    if globs.is_empty() {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| TEXT_EXT.contains(&e.to_lowercase().as_str()))
                    } else {
                        globs.iter().any(|g| glob_match(g, rel))
                    }
                });
                out.push_str(&format!("Folder with {} text files, for example:
", found.len()));
                for (_, rel) in found.iter().take(12) {
                    out.push_str(&format!("- {rel}
"));
                }
                if let Some((p, rel)) = found.first() {
                    out.push_str(&format!("Start of {rel}:
{}
", head(p, 700)));
                }
            }
            "file" => {
                let p = PathBuf::from(s["path"].as_str().unwrap_or(""));
                out.push_str(&format!("Start of the file:
{}
", head(&p, 900)));
            }
            "skill" => {
                let p = PathBuf::from(s["path"].as_str().unwrap_or("")).join("SKILL.md");
                out.push_str(&format!("A skill. Start of SKILL.md:
{}
", head(&p, 900)));
            }
            "url" => out.push_str(&format!("A web page: {}
", s["url"].as_str().unwrap_or(""))),
            "km" => out.push_str(&format!("The Knowledge Module {}
", s["id"].as_str().unwrap_or(""))),
            "tool" => out.push_str(&format!("The tool {}
", s["name"].as_str().unwrap_or(""))),
            "traces" => out.push_str(&format!("Past requests matching {:?}
", s["query"].as_str().unwrap_or(""))),
            _ => {}
        }
    }
    out
}

/// Reads every source of a task. `cancelled` is asked between sources.
pub async fn gather(
    engine: &Arc<Engine>,
    km: &Arc<Km>,
    sources: &[Value],
    limits: &Limits,
    default_license: Option<&str>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Res<Gathered> {
    let mut out = Gathered::default();
    for (i, src) in sources.iter().enumerate() {
        if cancelled() {
            return Err(Fail::new(499, "cancelled", "cancelled"));
        }
        let kind = src["kind"].as_str().unwrap_or("");
        let before = out.chunks.len();
        match kind {
            "dir" | "file" | "skill" | "km" | "traces" => {
                let (s, l, d) = (src.clone(), limits.clone(), default_license.map(str::to_string));
                let (k, home) = (km.clone(), engine.home.clone());
                let mut local = std::mem::take(&mut out);
                let kind_owned = kind.to_string();
                let r = tokio::task::spawn_blocking(move || {
                    let res = {
                        let mut sink = Sink {
                            out: &mut local,
                            limits: &l,
                        };
                        match kind_owned.as_str() {
                            "dir" => gather_dir(&s, &l, d.as_deref(), &mut sink),
                            "file" => gather_file(&s, &l, d.as_deref(), &mut sink),
                            "skill" => gather_skill(&s, &l, &mut sink),
                            "km" => gather_km(&k, &s, &mut sink),
                            _ => gather_traces(&home, &s, &mut sink),
                        }
                    };
                    (local, res)
                })
                .await
                .map_err(|e| Fail::new(500, "internal", e.to_string()))?;
                out = r.0;
                r.1.map_err(|e| Fail::new(e.status, e.code, format!("sources[{i}] ({kind}): {}", e.message)))?;
            }
            "url" => {
                let mut sink = Sink {
                    out: &mut out,
                    limits,
                };
                gather_url(engine, src, limits, default_license, &mut sink)
                    .await
                    .map_err(|e| Fail::new(e.status, e.code, format!("sources[{i}] (url): {}", e.message)))?;
            }
            "tool" => {
                let t = tool_def(src).map_err(|e| Fail::new(e.status, e.code, format!("sources[{i}] (tool): {}", e.message)))?;
                let ord = out.chunks.len() as i64;
                out.chunks.push(ChunkRow {
                    id: format!("c{}", ord + 1),
                    ord,
                    text: tool_text(&t),
                    origin: json!({"kind": "tool", "name": t.name, "provider": t.provider, "def": t.to_json()}),
                    role: "tool".into(),
                    emb: None,
                });
                out.tools.push(t);
            }
            other => return Err(Fail::invalid(format!("sources[{i}]: unknown kind {other:?}"))),
        }
        if out.chunks.len() == before {
            out.notes.push(format!("sources[{i}] ({kind}) gave no text"));
        }
    }
    // chunk ids are renumbered in order so they are stable and gap-free
    for (i, c) in out.chunks.iter_mut().enumerate() {
        c.ord = i as i64;
        c.id = format!("c{}", i + 1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_cross_folders_only_with_double_star() {
        assert!(glob_match("**/*.md", "docs/native/km-spec.md"));
        assert!(glob_match("*.md", "docs/a.MD"), "a bare pattern matches at any depth, ignoring case");
        assert!(glob_match("docs/*.md", "docs/a.md"));
        assert!(!glob_match("docs/*.md", "docs/native/a.md"));
        assert!(glob_match("docs/**", "docs/native/a.md"));
        assert!(glob_match("src/**/mod.rs", "src/mod.rs") && glob_match("src/**/mod.rs", "src/a/b/mod.rs"));
        assert!(!glob_match("*.rs", "a.md") && glob_match("a?c.txt", "abc.txt"));
    }

    #[test]
    fn chunks_keep_line_numbers_and_stay_small() {
        let text = (1..=40).map(|i| format!("line number {i}")).collect::<Vec<_>>().join("\n");
        let cs = chunk_text(&text, 100);
        assert!(cs.len() > 3);
        assert!(cs.iter().all(|c| c.0.chars().count() <= 100), "{:?}", cs.iter().map(|c| c.0.len()).collect::<Vec<_>>());
        assert_eq!(cs[0].1, 1);
        assert_eq!(cs.last().map(|c| c.2), Some(40));
        for w in cs.windows(2) {
            assert_eq!(w[1].1, w[0].2 + 1, "chunks follow each other without gaps");
        }
        assert!(chunk_text("   \n\n", 100).is_empty());
        let long = "x".repeat(250);
        assert_eq!(chunk_text(&long, 100).len(), 3, "an over-long line is cut");
    }

    #[test]
    fn paragraphs_are_kept_together_when_they_fit() {
        let text = "alpha alpha alpha alpha alpha\nbeta beta\n\ngamma gamma gamma gamma gamma gamma\ndelta";
        let cs = chunk_text(text, 50);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].0.contains("beta") && cs[1].0.starts_with("gamma"));
    }

    #[test]
    fn html_becomes_text_and_scripts_vanish() {
        let t = html_to_text("<html><script>alert(1)</script><body><h1>Hi &amp; bye</h1><p>one</p></body></html>");
        assert!(t.contains("Hi & bye") && t.contains("one") && !t.contains("alert"));
    }

    #[test]
    fn licences_are_read_from_headers_and_files() {
        assert_eq!(spdx_of("// SPDX-License-Identifier: MIT\nfn x(){}").as_deref(), Some("MIT"));
        assert_eq!(spdx_of("nothing here"), None);
        let d = crate::testing::temp_home();
        std::fs::write(d.join("LICENSE"), "Apache License\nVersion 2.0").unwrap();
        assert_eq!(license_file(&d).as_deref(), Some("Apache-2.0"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tools_come_from_hub_operations_or_inline_schemas() {
        let t = tool_def(&json!({"name": "km.list"})).unwrap();
        assert_eq!((t.provider.as_str(), t.side_effects.as_str()), ("hub", "read"));
        assert!(t.input_schema.is_object());
        assert_eq!(tool_def(&json!({"name": "km.import"})).unwrap().side_effects, "write");
        assert!(tool_def(&json!({"name": "no.such.op"})).unwrap_err().message.contains("not a hub operation"));
        let m = tool_def(&json!({"name": "grep", "provider": "mcp:files", "description": "d",
                                 "input_schema": {"type": "object"}}))
        .unwrap();
        assert_eq!(m.provider, "mcp:files");
        assert!(tool_def(&json!({"name": "x", "provider": "shell", "input_schema": {}})).is_err());
    }
}

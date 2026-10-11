//! Step 6, build: the KMs a task asked for are written as `mmkm/1` folders, checked with the KM runtime's own manifest
//! and archive code, packed into `.mmkm` files and imported into the store.
//!
//! * `data`: the records (km-spec.md section 8) that were not rejected;
//! * `memory`: the gathered chunks with their embeddings;
//! * `skill` (from a skill source) and `tool` (one per tool source): see km-factory.md section 4;
//! * `adapter`: only through the registered [`Trainer`](super::model::Trainer); without one it is `pending: forge`.
//!
//! Each KM gets its routing descriptor and centroid and the resources measured from its files. A version is never
//! changed: the evaluation step makes the next patch version with the uplift in `[quality]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::checks::ToolDef;
use super::embed::centroid;
use super::run::{Halt, Run, StepRes};
use super::store::{ArtifactRow, RecordRow};
use super::util::truncate;
use super::StepEnd;
use crate::engine::Fail;
use crate::km::embed::f32_to_f16;
use crate::km::manifest::{valid_id, Manifest};
use crate::km::pack::{self, Source};
use crate::km::vocab::Vocabulary;
use crate::km::{sha256_hex, Km};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TagDraft {
    pub tag: String,
    pub origin: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileSpec {
    pub path: String,
    pub role: String,
    pub format: String,
}

/// Everything in a `km.toml` except the file bytes (those are in the folder).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub domain: String,
    pub tags: Vec<TagDraft>,
    pub license: Option<String>,
    pub created: String,
    pub parent: Option<String>,
    pub requires: Vec<String>,
    pub base: String,
    pub arch: Option<String>,
    pub base_hash: Option<String>,
    pub descriptor: String,
    pub keywords: Vec<String>,
    pub embed_model: Option<String>,
    pub eval_set: Option<String>,
    pub eval_hash: Option<String>,
    pub scores: BTreeMap<String, f64>,
    pub training_run: String,
    pub teachers: Vec<String>,
    pub data: Vec<String>,
    /// The kind's own table, as TOML text.
    pub kind_table: String,
    pub files: Vec<FileSpec>,
}

impl Draft {
    pub fn key(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn list(v: &[String]) -> String {
    format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "))
}

/// Writes `<root>/<id>@<version>/` from a draft and the bytes of its files; returns the folder.
pub fn write_dir(d: &Draft, root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<PathBuf, Fail> {
    let io = |p: &Path, e: std::io::Error| Fail::new(500, "io", format!("{}: {e}", p.display()));
    let dir = root.join(d.key());
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| io(&dir, e))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let mut t = String::new();
    t.push_str(&format!(
        "spec = \"mmkm/1\"\nid = {}\nversion = {}\nkind = {}\nname = {}\ndescription = {}\ndomain = {}\ntags = {}\n",
        q(&d.id),
        q(&d.version),
        q(&d.kind),
        q(&d.name),
        q(&d.description),
        q(&d.domain),
        list(&d.tags.iter().map(|t| t.tag.clone()).collect::<Vec<_>>())
    ));
    if let Some(l) = &d.license {
        t.push_str(&format!("license = {}\n", q(l)));
    }
    t.push_str(&format!("created = {}\n", d.created));
    if let Some(p) = &d.parent {
        t.push_str(&format!("parent = {}\n", q(p)));
    }
    if !d.requires.is_empty() {
        t.push_str(&format!("requires = {}\n", list(&d.requires)));
    }
    t.push_str(&format!("\n[compat]\nbase = {}\n", q(&d.base)));
    if let Some(a) = &d.arch {
        t.push_str(&format!("arch = {}\n", q(a)));
    }
    if let Some(h) = &d.base_hash {
        t.push_str(&format!("base_hash = {}\n", q(h)));
    }
    t.push_str("\n[modalities]\ninput = [\"text\"]\noutput = [\"text\"]\n");
    t.push_str(&format!(
        "\n[routing]\ndescriptor = {}\nkeywords = {}\nprior = 0.0\n",
        q(&d.descriptor),
        list(&d.keywords)
    ));
    if let Some(m) = &d.embed_model {
        t.push_str(&format!("embed_model = {}\n", q(m)));
    }
    if d.files.iter().any(|f| f.role == "centroid") {
        t.push_str("centroid = \"payload/centroid.f16\"\n");
    }
    let total: u64 = d.files.iter().map(|f| files.get(&f.path).map_or(0, |b| b.len() as u64)).sum();
    t.push_str(&format!("\n[resources]\nram_bytes = {total}\nvram_bytes = {}\n", if d.kind == "adapter" { total } else { 0 }));
    if d.eval_set.is_some() || !d.scores.is_empty() {
        t.push_str("\n[quality]\n");
        if let Some(e) = &d.eval_set {
            t.push_str(&format!("eval_set = {}\n", q(e)));
        }
        if let Some(h) = &d.eval_hash {
            t.push_str(&format!("eval_hash = {}\n", q(h)));
        }
        if !d.scores.is_empty() {
            let s = d.scores.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join(", ");
            t.push_str(&format!("scores = {{ {s} }}\n"));
        }
    }
    t.push_str(&format!(
        "\n[provenance]\ncreated_by = \"auto\"\ntraining_run = {}\nteachers = {}\ndata = {}\n",
        q(&d.training_run),
        list(&d.teachers),
        list(&d.data)
    ));
    if !d.tags.is_empty() {
        t.push_str("\n[tag_info]\n");
        for g in &d.tags {
            t.push_str(&format!("{} = {{ origin = {}, confidence = {} }}\n", q(&g.tag), q(&g.origin), g.confidence));
        }
    }
    if !d.kind_table.is_empty() {
        t.push('\n');
        t.push_str(&d.kind_table);
        t.push('\n');
    }
    for f in &d.files {
        let bytes = files
            .get(&f.path)
            .ok_or_else(|| Fail::new(500, "internal", format!("{}: no bytes for {}", d.key(), f.path)))?;
        let full = dir.join(&f.path);
        std::fs::create_dir_all(full.parent().unwrap_or(&dir)).map_err(|e| io(&full, e))?;
        std::fs::write(&full, bytes).map_err(|e| io(&full, e))?;
        if f.role == "eval" {
            continue; // named by [quality] eval_set and pinned by eval_hash; not a payload file
        }
        t.push_str(&format!(
            "\n[[payload]]\npath = {}\nrole = {}\nformat = {}\nbytes = {}\nsha256 = \"{}\"\n",
            q(&f.path),
            q(&f.role),
            q(&f.format),
            bytes.len(),
            sha256_hex(bytes)
        ));
    }
    std::fs::write(dir.join("km.toml"), t).map_err(|e| io(&dir, e))?;
    Ok(dir)
}

/// Reads the files of an already written draft back (for the next version of it).
pub fn read_files(d: &Draft, dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, Fail> {
    d.files
        .iter()
        .map(|f| {
            std::fs::read(dir.join(&f.path))
                .map(|b| (f.path.clone(), b))
                .map_err(|e| Fail::new(500, "io", format!("{}/{}: {e}", dir.display(), f.path)))
        })
        .collect()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The version a new KM id gets: 0.1.0, or the minor after the newest installed one (with that one as parent).
fn fresh_version(km: &Km, id: &str) -> (String, Option<String>) {
    match km.store.versions(id).ok().and_then(|v| v.into_iter().next()) {
        Some(newest) => {
            let v = newest.version();
            (format!("{}.{}.0", v.major, v.minor + 1), Some(newest.key))
        }
        None => ("0.1.0".into(), None),
    }
}

/// Validates, packs and imports a folder written by [`write_dir`]. Blocking.
pub fn pack_and_import(km: &Km, dir: &Path, out_dir: &Path) -> Result<Value, Fail> {
    let m = Manifest::from_file(&dir.join("km.toml"))?;
    pack::verify_dir(dir, &m)?;
    std::fs::create_dir_all(out_dir).map_err(|e| Fail::new(500, "io", format!("{}: {e}", out_dir.display())))?;
    let out = out_dir.join(format!("{}.mmkm", m.key()));
    pack::pack_dir(dir, &out)?;
    let mut r = km.import_sync(&out.to_string_lossy(), Source::Own, false, None)?;
    r["archive"] = json!(out.file_name().map(|n| n.to_string_lossy().into_owned()));
    Ok(r)
}

/// Tags of a set of records as KM tags: the ones enough records carry, with the mean confidence they had.
pub fn km_tags(rows: &[&RecordRow], plan_tags: &[String], vocab: &Vocabulary, extra: &[String]) -> Vec<TagDraft> {
    let n = rows.len().max(1) as f64;
    let mut by: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for r in rows {
        if let Some(info) = r.json["tag_info"].as_object() {
            for (t, i) in info {
                by.entry(t.clone())
                    .or_default()
                    .push((i["origin"].as_str().unwrap_or("classifier").to_string(), i["confidence"].as_f64().unwrap_or(0.5)));
            }
        }
    }
    let mut out: Vec<TagDraft> = by
        .into_iter()
        .filter(|(_, v)| v.len() as f64 / n >= 0.3)
        .map(|(tag, v)| {
            let freq = v.len() as f64 / n;
            let mean = v.iter().map(|x| x.1).sum::<f64>() / v.len() as f64;
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            v.iter().for_each(|x| *counts.entry(x.0.as_str()).or_default() += 1);
            let origin = counts.into_iter().max_by_key(|c| c.1).map_or("classifier", |c| c.0).to_string();
            TagDraft {
                tag,
                origin,
                confidence: ((mean * freq.sqrt()).min(1.0) * 1000.0).round() / 1000.0,
            }
        })
        .collect();
    for t in plan_tags.iter().chain(extra) {
        if !out.iter().any(|o| &o.tag == t) {
            out.push(TagDraft {
                tag: t.clone(),
                origin: "author_model".into(),
                confidence: 0.6,
            });
        }
    }
    out.retain(|t| vocab.problem(&t.tag).is_none());
    out
}

struct Ctx<'a> {
    run: &'a Run,
    plan: Value,
    domain: String,
    tags: Vec<TagDraft>,
    embed_model: String,
    descriptor: String,
    keywords: Vec<String>,
    license: Option<String>,
    teachers: Vec<String>,
}

fn centroid_file(vs: &[&[f32]]) -> Option<Vec<u8>> {
    centroid(vs).map(|c| f32_to_f16(&c))
}

impl Ctx<'_> {
    fn draft(&self, kind: &str, id: &str, name: &str, description: &str) -> Draft {
        let (version, parent) = fresh_version(&self.run.km, id);
        Draft {
            id: id.into(),
            version,
            kind: kind.into(),
            name: name.into(),
            description: description.into(),
            domain: self.domain.clone(),
            tags: self.tags.clone(),
            license: self.license.clone(),
            created: now_iso(),
            parent,
            requires: vec![],
            base: "any".into(),
            arch: None,
            base_hash: None,
            descriptor: self.descriptor.clone(),
            keywords: self.keywords.clone(),
            embed_model: Some(self.embed_model.clone()),
            eval_set: None,
            eval_hash: None,
            scores: BTreeMap::new(),
            training_run: self.run.id.clone(),
            teachers: self.teachers.clone(),
            data: vec![],
            kind_table: String::new(),
            files: vec![],
        }
    }
}

fn add_file(d: &mut Draft, files: &mut BTreeMap<String, Vec<u8>>, path: &str, role: &str, format: &str, bytes: Vec<u8>) {
    d.files.push(FileSpec {
        path: path.into(),
        role: role.into(),
        format: format.into(),
    });
    files.insert(path.into(), bytes);
}

fn add_centroid(d: &mut Draft, files: &mut BTreeMap<String, Vec<u8>>, c: Option<Vec<u8>>) {
    match c {
        Some(b) => add_file(d, files, "payload/centroid.f16", "centroid", "f16", b),
        None => d.embed_model = None,
    }
}

fn build_data(c: &Ctx, rows: &[&RecordRow]) -> Result<(Draft, BTreeMap<String, Vec<u8>>), Fail> {
    let mut files = BTreeMap::new();
    let mut jsonl = String::new();
    for r in rows {
        let mut rec = r.json.clone();
        rec["status"] = json!(r.status);
        rec["split"] = json!(r.split);
        jsonl.push_str(&serde_json::to_string(&rec).unwrap_or_default());
        jsonl.push('\n');
    }
    let id = format!("data.{}", c.run.spec.name_slug());
    let mut d = c.draft(
        "data",
        &id,
        &format!("{} (records)", plan_title(&c.plan, &c.run.spec.goal)),
        &format!("Training records for: {}", truncate(&c.run.spec.goal, 200)),
    );
    add_file(&mut d, &mut files, "payload/records.jsonl", "records", "jsonl", jsonl.into_bytes());
    let embs: Vec<&[f32]> = rows.iter().filter(|r| r.split != "heldout").filter_map(|r| r.emb.as_deref()).collect();
    add_centroid(&mut d, &mut files, centroid_file(&embs));
    Ok((d, files))
}

fn plan_title(plan: &Value, goal: &str) -> String {
    plan["title"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| truncate(goal, 60))
}

fn build_memory(c: &Ctx) -> Result<(Draft, BTreeMap<String, Vec<u8>>), Fail> {
    let chunks: Vec<_> = c
        .run
        .f
        .db
        .chunks(&c.run.id)
        .into_iter()
        .filter(|ch| ch.role != "tool" && ch.emb.is_some())
        .collect();
    if chunks.is_empty() {
        return Err(Fail::invalid("no embedded chunks to make a memory KM from"));
    }
    let dim = chunks[0].emb.as_ref().map_or(0, Vec::len);
    let mut jsonl = String::new();
    let mut emb: Vec<u8> = vec![];
    for ch in &chunks {
        let src = match (ch.origin["path"].as_str(), ch.origin["url"].as_str()) {
            (Some(p), _) => format!("{p}:{}-{}", ch.origin["line_start"], ch.origin["line_end"]),
            (_, Some(u)) => u.to_string(),
            _ => ch.origin["kind"].as_str().unwrap_or("source").to_string(),
        };
        let mut origin = ch.origin.clone();
        if let Some(o) = origin.as_object_mut() {
            o.remove("def");
        }
        jsonl.push_str(&json!({"id": ch.id, "text": ch.text, "source": src, "origin": origin}).to_string());
        jsonl.push('\n');
        emb.extend(f32_to_f16(ch.emb.as_deref().unwrap_or(&[])));
    }
    let id = format!("memory.{}", c.run.spec.name_slug());
    let mut files = BTreeMap::new();
    let mut d = c.draft(
        "memory",
        &id,
        &format!("{} (notes)", plan_title(&c.plan, &c.run.spec.goal)),
        &format!("Source notes for: {}", truncate(&c.run.spec.goal, 200)),
    );
    d.kind_table = format!(
        "[memory]\nchunks = \"payload/chunks.jsonl\"\nembeddings = \"payload/emb.f16\"\ndim = {dim}\nindex = \"flat\""
    );
    add_file(&mut d, &mut files, "payload/chunks.jsonl", "chunks", "jsonl", jsonl.into_bytes());
    add_file(&mut d, &mut files, "payload/emb.f16", "embeddings", "f16", emb);
    let embs: Vec<&[f32]> = chunks.iter().filter_map(|ch| ch.emb.as_deref()).collect();
    add_centroid(&mut d, &mut files, centroid_file(&embs));
    Ok((d, files))
}

fn build_tool(c: &Ctx, t: &ToolDef, rows: &[&RecordRow]) -> Result<(Draft, BTreeMap<String, Vec<u8>>), Fail> {
    let id = format!("tool.{}", super::util::slug(&t.name));
    if !valid_id(&id) {
        return Err(Fail::invalid(format!("tool {:?} cannot be an id ({id})", t.name)));
    }
    let mut files = BTreeMap::new();
    let mut d = c.draft("tool", &id, &format!("Tool {}", t.name), &truncate(&t.description, 300));
    d.description = if d.description.is_empty() { format!("Use of the tool {}", t.name) } else { d.description };
    let mut ex = String::new();
    for r in rows {
        for call in r.json["tool_calls"].as_array().into_iter().flatten().filter(|x| x["name"] == t.name.as_str()) {
            ex.push_str(
                &json!({"input": super::verify::user_text(&r.json), "call": call, "output": r.json["output"],
                        "record": r.id})
                .to_string(),
            );
            ex.push('\n');
        }
    }
    let side = t.side_effects.as_str();
    d.kind_table = format!(
        "[tool]\nprovider = {}\nname = {}\ndescription = {}\nside_effects = {}\ninput_schema = {}{}",
        q(&t.provider),
        q(&t.name),
        q(&truncate(&t.description, 500)),
        q(if matches!(side, "none" | "read" | "write") { side } else { "none" }),
        q(&serde_json::to_string(&t.input_schema).unwrap_or_default()),
        if ex.is_empty() { "" } else { "\nexamples = \"payload/examples.jsonl\"" }
    );
    add_file(
        &mut d,
        &mut files,
        "payload/schema.json",
        "schema",
        "json",
        serde_json::to_vec_pretty(&t.to_json()).unwrap_or_default(),
    );
    if !ex.is_empty() {
        add_file(&mut d, &mut files, "payload/examples.jsonl", "examples", "jsonl", ex.into_bytes());
    }
    let tool_tag = format!("tool:{}", super::util::slug(&t.name).replace('.', "-"));
    if !d.tags.iter().any(|g| g.tag == tool_tag) {
        d.tags.push(TagDraft {
            tag: tool_tag,
            origin: "classifier".into(),
            confidence: 0.95,
        });
    }
    let emb = c
        .run
        .f
        .db
        .chunks(&c.run.id)
        .into_iter()
        .find(|ch| ch.role == "tool" && ch.origin["name"] == t.name.as_str())
        .and_then(|ch| ch.emb);
    add_centroid(&mut d, &mut files, emb.as_deref().and_then(|e| centroid_file(&[e])));
    Ok((d, files))
}

fn build_skill(c: &Ctx, data_key: Option<&str>, tool_ids: &[String]) -> Result<(Draft, BTreeMap<String, Vec<u8>>), Fail> {
    let src = c
        .run
        .spec
        .sources
        .iter()
        .find(|s| s["kind"] == "skill")
        .ok_or_else(|| Fail::invalid("no skill source"))?;
    let root = PathBuf::from(src["path"].as_str().unwrap_or(""));
    let instr = std::fs::read(root.join("SKILL.md"))
        .map_err(|e| Fail::invalid(format!("skill source {}: {e}", root.display())))?;
    let mut files = BTreeMap::new();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "skill".into());
    let id = format!("skill.{}", super::util::slug(&name).replace('.', "-"));
    let mut d = c.draft(
        "skill",
        &id,
        &format!("Skill {name}"),
        &format!("The skill {name}: instructions, examples and tool references"),
    );
    let mut table = String::from("[skill]\ninstructions = \"payload/SKILL.md\"\n");
    if let Some(k) = data_key {
        table.push_str(&format!("examples = {}\n", q(k)));
    }
    table.push_str(&format!("tools = {}", list(tool_ids)));
    d.kind_table = table;
    add_file(&mut d, &mut files, "payload/SKILL.md", "instructions", "md", instr);
    // the folder's other text files travel with it (small ones only)
    let mut extra = vec![];
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.filter_map(Result::ok) {
            let n = e.file_name().to_string_lossy().into_owned();
            let ok_ext = ["md", "txt", "json", "yaml", "yml", "toml"].contains(&n.rsplit('.').next().unwrap_or("").to_lowercase().as_str());
            if n != "SKILL.md" && ok_ext && e.metadata().is_ok_and(|m| m.is_file() && m.len() < 256 * 1024) {
                extra.push(n);
            }
        }
    }
    extra.sort();
    for n in extra.into_iter().take(20) {
        if let Ok(b) = std::fs::read(root.join(&n)) {
            let ext = n.rsplit('.').next().unwrap_or("txt").to_lowercase();
            add_file(&mut d, &mut files, &format!("payload/files/{n}"), "file", &ext, b);
        }
    }
    let embs: Vec<Vec<f32>> = c
        .run
        .f
        .db
        .chunks(&c.run.id)
        .into_iter()
        .filter(|ch| ch.role == "skill")
        .filter_map(|ch| ch.emb)
        .collect();
    let refs: Vec<&[f32]> = embs.iter().map(Vec::as_slice).collect();
    add_centroid(&mut d, &mut files, centroid_file(&refs));
    if !tool_ids.is_empty() {
        d.requires = tool_ids.to_vec();
    }
    Ok((d, files))
}

/// Writes, validates, packs and imports one KM; records it as an artifact. Blocking work runs off the runtime.
pub async fn publish_version(
    run: &Run,
    d: Draft,
    files: BTreeMap<String, Vec<u8>>,
    stage: &str,
) -> StepRes<Value> {
    let build_dir = run.dir().join("build");
    let out_dir = run.dir().join("out");
    // remembered before it is imported, so a crash here leaves the same version to finish
    let mut row = ArtifactRow {
        id: d.id.clone(),
        version: d.version.clone(),
        kind: d.kind.clone(),
        stage: stage.into(),
        draft: serde_json::to_value(&d).unwrap_or(Value::Null),
        key: None,
        content_id: None,
        imported: false,
    };
    run.f.db.put_artifact(&run.id, &row)?;
    let km: std::sync::Arc<Km> = run.km.clone();
    let d2 = d.clone();
    let (bd, od) = (build_dir.clone(), out_dir.clone());
    let res = tokio::task::spawn_blocking(move || -> Result<Value, Fail> {
        let dir = write_dir(&d2, &bd, &files)?;
        pack_and_import(&km, &dir, &od)
    })
    .await
    .map_err(|e| Halt::Fail(Fail::new(500, "internal", e.to_string())))?
    .map_err(|e| Halt::Fail(Fail::new(e.status, e.code, format!("{}: {}", d.key(), e.message))))?;
    row.key = res["key"].as_str().map(str::to_string);
    row.content_id = res["content_id"].as_str().map(str::to_string);
    row.imported = true;
    run.f.db.put_artifact(&run.id, &row)?;
    run.log("info", &format!("built {} ({} bytes)", d.key(), res["bytes"]));
    Ok(json!({"id": d.id, "version": d.version, "kind": d.kind, "key": d.key(), "content_id": res["content_id"],
              "bytes": res["bytes"], "tags": d.tags.len(), "unapproved_tags": res["unapproved_tags"],
              "warnings": res["warnings"], "already_installed": res["already_installed"]}))
}

fn find_built<'a>(existing: &'a [ArtifactRow], id: &str) -> Option<&'a ArtifactRow> {
    existing.iter().find(|a| a.id == id && a.stage == "built" && a.imported)
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let plan = run.plan()?;
    let spec = &run.spec;
    let vocab = run.km.vocabulary()?;
    let rows_all = run.f.db.records(&run.id, None);
    let rows: Vec<&RecordRow> = rows_all.iter().filter(|r| r.status != "rejected").collect();
    let plan_tags: Vec<String> = plan["taxonomy"]["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let domain = {
        let d = plan["taxonomy"]["domain"].as_str().unwrap_or("").trim_matches('/').to_string();
        if d.is_empty() { spec.domain.trim_matches('/').to_string() } else { d }
    };
    let licenses: Vec<String> = {
        let mut l: Vec<String> = run
            .f
            .db
            .chunks(&run.id)
            .iter()
            .filter_map(|c| c.origin["license"].as_str().map(str::to_string))
            .collect();
        l.sort();
        l.dedup();
        l
    };
    let keywords: Vec<String> = plan["keywords"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(12)
        .map(str::to_string)
        .collect();
    let descriptor = truncate(
        &format!(
            "{} {} {}",
            plan_title(&plan, &spec.goal),
            plan["summary"].as_str().unwrap_or(""),
            keywords.join(" ")
        ),
        500,
    );
    let c = Ctx {
        run,
        plan: plan.clone(),
        domain,
        tags: km_tags(&rows, &plan_tags, &vocab, &[]),
        embed_model: run.embedder().model_id(),
        descriptor,
        keywords,
        license: if licenses.len() == 1 { licenses.first().cloned() } else { spec.license.clone() },
        teachers: vec![spec.author.clone()],
    };
    let existing = run.f.db.artifacts(&run.id);
    let have = |id: &str| find_built(&existing, id);
    let mut built: Vec<Value> = vec![];
    let mut notes: Vec<String> = vec![];
    let mut adapter = Value::Null;
    let mut data_key: Option<String> = None;

    let mut add = |v: Value| built.push(v);

    // tools first (a skill refers to them), then data (a skill's examples), memory, skill, adapter
    let mut tool_ids = vec![];
    if spec.wants("tool") {
        for t in run.tools() {
            run.check()?;
            let id = format!("tool.{}", super::util::slug(&t.name));
            tool_ids.push(id.clone());
            if let Some(a) = have(&id) {
                add(json!({"id": a.id, "version": a.version, "kind": "tool", "key": a.key, "reused": true}));
                continue;
            }
            let tool_rows: Vec<&RecordRow> = rows.iter().copied().filter(|r| r.json["task"] == "tool_use").collect();
            let (d, f) = build_tool(&c, &t, &tool_rows)?;
            add(publish_version(run, d, f, "built").await?);
        }
    }
    if spec.wants("data") || spec.wants("skill") {
        let id = format!("data.{}", spec.name_slug());
        if let Some(a) = have(&id) {
            data_key = a.key.clone();
            if spec.wants("data") {
                add(json!({"id": a.id, "version": a.version, "kind": "data", "key": a.key, "reused": true}));
            }
        } else if rows.is_empty() {
            return Err(Halt::Fail(Fail::new(422, "no_records", "no record survived verification; nothing to build a data KM from")));
        } else {
            let (d, f) = build_data(&c, &rows)?;
            data_key = Some(d.key());
            add(publish_version(run, d, f, "built").await?);
        }
    }
    if spec.wants("memory") {
        let id = format!("memory.{}", spec.name_slug());
        if let Some(a) = have(&id) {
            add(json!({"id": a.id, "version": a.version, "kind": "memory", "key": a.key, "reused": true}));
        } else {
            let (d, f) = build_memory(&c)?;
            add(publish_version(run, d, f, "built").await?);
        }
    }
    if spec.wants("skill") {
        let (mut d, f) = build_skill(&c, data_key.as_deref(), &tool_ids)?;
        if let Some(a) = have(&d.id) {
            add(json!({"id": a.id, "version": a.version, "kind": "skill", "key": a.key, "reused": true}));
        } else {
            if let Some(k) = &data_key {
                d.data = vec![k.clone()];
            }
            add(publish_version(run, d, f, "built").await?);
        }
    }
    if spec.wants("adapter") {
        adapter = build_adapter(&c, &rows, data_key.as_deref(), &mut notes, &existing).await?;
        if adapter["key"].is_string() {
            add(adapter.clone());
        }
    }
    let unapproved: Vec<Value> = built.iter().flat_map(|b| b["unapproved_tags"].as_array().cloned().unwrap_or_default()).collect();
    Ok(StepEnd::Done(json!({"kms": built, "adapter": if adapter.is_null() { Value::Null } else { adapter["status"].clone() },
                            "unapproved_tags": unapproved, "notes": notes})))
}

async fn build_adapter(
    c: &Ctx<'_>,
    rows: &[&RecordRow],
    data_key: Option<&str>,
    notes: &mut Vec<String>,
    existing: &[ArtifactRow],
) -> StepRes<Value> {
    let run = c.run;
    let id = format!("adapter.{}", run.spec.name_slug());
    if let Some(a) = find_built(existing, &id) {
        return Ok(json!({"id": a.id, "version": a.version, "kind": "adapter", "key": a.key, "reused": true, "status": "built"}));
    }
    let Some(trainer) = run.f.trainer() else {
        notes.push("adapter KM: pending: forge (no trainer is registered; the Forge provides LoRA training)".into());
        return Ok(json!({"status": "pending: forge", "id": id}));
    };
    let Some(data_key) = data_key else {
        return Err(Halt::Fail(Fail::invalid("an adapter KM needs the data KM it is trained from (add kind data)")));
    };
    // training records: approved, split train
    let dir = run.dir().join("train");
    std::fs::create_dir_all(&dir).map_err(|e| Halt::Fail(Fail::new(500, "io", e.to_string())))?;
    let path = dir.join("records.jsonl");
    let body: String = rows
        .iter()
        .filter(|r| r.status == "approved" && r.split == "train")
        .map(|r| format!("{}\n", r.json))
        .collect();
    if body.is_empty() {
        notes.push("adapter KM: no approved training records yet".into());
        return Ok(json!({"status": "pending: approved records", "id": id}));
    }
    std::fs::write(&path, body).map_err(|e| Halt::Fail(Fail::new(500, "io", e.to_string())))?;
    run.check()?;
    let out = trainer
        .train(
            &run.hub,
            super::model::TrainRequest {
                task: run.id.clone(),
                km_id: id.clone(),
                base: run.spec.base_model(),
                data_km: data_key.to_string(),
                records: path,
                out_dir: dir.join("adapter"),
                device: run.spec.dev("base"),
                scope: crate::trace::Scope::new(Some(&run.job_id), Some(&run.id)),
            },
        )
        .await?;
    let bytes = std::fs::read(&out.adapter)
        .map_err(|e| Halt::Fail(Fail::new(500, "io", format!("{}: {e}", out.adapter.display()))))?;
    let mut d = c.draft("adapter", &id, &format!("{} (adapter)", plan_title(&c.plan, &run.spec.goal)), &format!("LoRA trained on: {}", truncate(&run.spec.goal, 200)));
    d.base = out.core.clone();
    d.arch = out.arch.clone();
    d.base_hash = out.base_hash.clone();
    d.data = vec![data_key.to_string()];
    d.training_run = out.run_id.clone();
    let mut files = BTreeMap::new();
    add_file(&mut d, &mut files, "payload/adapter.gguf", "lora", "gguf", bytes);
    let mut r = publish_version(run, d, files, "built").await?;
    r["status"] = json!("built");
    r["metrics"] = out.metrics;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_draft_is_a_valid_manifest_that_packs_and_verifies() {
        let root = crate::testing::temp_home();
        let mut d = Draft {
            id: "data.demo".into(),
            version: "0.1.0".into(),
            kind: "data".into(),
            name: "Demo \"quoted\"".into(),
            description: "d".into(),
            domain: "code/rust".into(),
            tags: vec![TagDraft {
                tag: "lang:rust".into(),
                origin: "classifier".into(),
                confidence: 0.8,
            }],
            license: Some("MIT".into()),
            created: "2026-10-10T12:00:00Z".into(),
            parent: None,
            requires: vec![],
            base: "any".into(),
            arch: None,
            base_hash: None,
            descriptor: "rust async".into(),
            keywords: vec!["tokio".into()],
            embed_model: None,
            eval_set: None,
            eval_hash: None,
            scores: BTreeMap::from([("uplift".to_string(), 0.25)]),
            training_run: "ft-1".into(),
            teachers: vec!["m".into()],
            data: vec![],
            kind_table: String::new(),
            files: vec![],
        };
        let mut files = BTreeMap::new();
        add_file(&mut d, &mut files, "payload/records.jsonl", "records", "jsonl", b"{\"id\":\"r-1\"}\n".to_vec());
        let dir = write_dir(&d, &root, &files).unwrap();
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        assert_eq!(m.key(), "data.demo@0.1.0");
        assert_eq!(m.quality.scores["uplift"], 0.25);
        assert_eq!(m.tag_meta["lang:rust"].origin, "classifier");
        assert_eq!(m.provenance.created_by, "auto");
        pack::verify_dir(&dir, &m).unwrap();
        // the next version reads the same bytes back
        assert_eq!(read_files(&d, &dir).unwrap(), files);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn km_tags_keep_frequent_valid_tags() {
        let mk = |tags: &[(&str, f64)]| RecordRow {
            id: "r".into(),
            ord: 1,
            json: json!({"tag_info": tags.iter().map(|(t, c)| (t.to_string(), json!({"origin": "classifier", "confidence": c}))).collect::<serde_json::Map<_, _>>()}),
            status: "approved".into(),
            reason: None,
            verified: true,
            tagged: true,
            hash: String::new(),
            score: 1.0,
            split: "train".into(),
            emb: None,
        };
        let a = mk(&[("lang:rust", 0.9), ("task:debug", 0.7), ("bogus:x", 0.9)]);
        let b = mk(&[("lang:rust", 0.8)]);
        let c = mk(&[("lang:rust", 0.8)]);
        let d = mk(&[("lang:rust", 0.8)]);
        let rows = vec![&a, &b, &c, &d];
        let tags = km_tags(&rows, &["task:plan".to_string()], &Vocabulary::default(), &[]);
        let names: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
        assert!(names.contains(&"lang:rust") && names.contains(&"task:plan"), "{names:?}");
        assert!(!names.contains(&"task:debug"), "one record in four is too few");
        assert!(!names.contains(&"bogus:x"), "outside the vocabulary");
    }
}

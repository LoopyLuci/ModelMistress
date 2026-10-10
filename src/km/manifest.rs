//! `km.toml`: parsing and validation (km-spec.md §4, §5).
//!
//! The typed [`Manifest`] holds what the runtime uses; the whole parsed document stays in `raw`, so keys this version
//! does not know are kept (and listed in `unknown`) rather than lost. The file's bytes are never rewritten: the
//! content id is the SHA-256 of exactly those bytes.

use std::collections::{BTreeMap, BTreeSet};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{sha256_hex, Res};
use crate::engine::Fail;

pub const SPEC: &str = "mmkm/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Adapter,
    Experts,
    Memory,
    Data,
    Specialist,
    Projector,
    Router,
    Skill,
    Tool,
    Bundle,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Adapter,
        Kind::Experts,
        Kind::Memory,
        Kind::Data,
        Kind::Specialist,
        Kind::Projector,
        Kind::Router,
        Kind::Skill,
        Kind::Tool,
        Kind::Bundle,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Adapter => "adapter",
            Kind::Experts => "experts",
            Kind::Memory => "memory",
            Kind::Data => "data",
            Kind::Specialist => "specialist",
            Kind::Projector => "projector",
            Kind::Router => "router",
            Kind::Skill => "skill",
            Kind::Tool => "tool",
            Kind::Bundle => "bundle",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Kinds that are tied to the exact weights of one base (they need `compat.base` and ideally `base_hash`).
    pub fn needs_base(self) -> bool {
        matches!(self, Kind::Adapter | Kind::Experts | Kind::Projector | Kind::Router)
    }

    /// The most payload bytes a KM of this kind may have (km-spec.md §9, size limits per kind).
    pub fn max_bytes(self) -> u64 {
        const GIB: u64 = 1 << 30;
        match self {
            Kind::Adapter => 4 * GIB,
            Kind::Experts => 64 * GIB,
            Kind::Memory => 32 * GIB,
            Kind::Data => 16 * GIB,
            Kind::Specialist => 1 << 20,
            Kind::Projector => 8 * GIB,
            Kind::Router => 2 * GIB,
            Kind::Skill => GIB / 4,
            Kind::Tool => 16 << 20,
            Kind::Bundle => 1 << 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compat {
    pub base: String,
    pub base_version: Option<String>,
    pub arch: Option<String>,
    pub base_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Routing {
    pub descriptor: String,
    pub embed_model: Option<String>,
    pub centroid: Option<String>,
    pub keywords: Vec<String>,
    pub intents: Vec<String>,
    pub prior: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Resources {
    pub ram_bytes: Option<u64>,
    pub vram_bytes: Option<u64>,
    pub load_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quality {
    pub eval_set: Option<String>,
    pub eval_hash: Option<String>,
    pub scores: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    pub created_by: String,
    pub training_run: Option<String>,
    pub teachers: Vec<String>,
    pub data: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub path: String,
    pub role: String,
    pub format: String,
    pub bytes: u64,
    pub sha256: String,
    /// experts: the layer this file holds, and the slot range `[first, last]`.
    pub layer: Option<u32>,
    pub slots: Option<[u32; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpertsT {
    pub layers: Vec<u32>,
    pub slots_per_layer: u32,
    pub hidden: Option<u32>,
    pub ffn: Option<u32>,
    pub gate: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryT {
    pub chunks: String,
    pub embeddings: String,
    pub dim: usize,
    pub index: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpecialistT {
    pub engine: String,
    pub model: String,
    pub tool_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectorT {
    pub from: String,
    pub feature_dim: u32,
    pub tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillT {
    pub instructions: String,
    /// A data KM (`id@range`) holding the examples.
    pub examples: Option<String>,
    /// Tool KM ids the skill uses.
    pub tools: Vec<String>,
}

/// A tool KM only refers to a tool the host provides. It has no field that could grant anything.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolT {
    /// `hub` (a hub operation) or `mcp:<server>`.
    pub provider: String,
    pub name: String,
    pub description: String,
    pub examples: Option<String>,
    /// none | read | write
    pub side_effects: String,
}

/// Consolidation of a KM into one core's weights (km-factory.md section 3).
#[derive(Debug, Clone, PartialEq)]
pub struct Consolidation {
    /// none | learning | absorbed
    pub state: String,
    pub uplift: Option<f64>,
    pub last_step: Option<String>,
    pub steps: u64,
}

impl Consolidation {
    pub fn to_json(&self) -> Value {
        json!({"state": self.state, "uplift": self.uplift, "last_step": self.last_step, "steps": self.steps})
    }
}

/// Where a tag came from and how sure that is.
#[derive(Debug, Clone, PartialEq)]
pub struct TagMeta {
    /// manual | owner | classifier | nearest | author_model | import
    pub origin: String,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub id: String,
    pub version: Version,
    pub uuid: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub description: String,
    pub domain: String,
    pub tags: Vec<String>,
    pub license: Option<String>,
    pub created: Option<String>,
    pub parent: Option<String>,
    pub requires: Vec<String>,
    pub conflicts: Vec<String>,
    pub compat: Compat,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub routing: Routing,
    pub resources: Resources,
    pub quality: Quality,
    pub provenance: Provenance,
    pub payload: Vec<Payload>,
    pub experts: Option<ExpertsT>,
    pub memory: Option<MemoryT>,
    pub specialist: Option<SpecialistT>,
    pub projector: Option<ProjectorT>,
    pub members: Vec<String>,
    pub skill: Option<SkillT>,
    pub tool: Option<ToolT>,
    /// Per core (`dusty`, `misty`, `any`).
    pub consolidation: BTreeMap<String, Consolidation>,
    /// Every tag with its origin and confidence (tags without an entry in `tag_info` are manual, 1.0).
    pub tag_meta: BTreeMap<String, TagMeta>,
    /// sha256 of the km.toml bytes, lowercase hex.
    pub content_id: String,
    /// Keys this version does not know, as `table.key` paths. They are kept in `raw` and ignored.
    pub unknown: Vec<String>,
    /// Things that are allowed but worth saying (a base-bound KM without `base_hash`, a table its kind ignores).
    pub warnings: Vec<String>,
    /// The whole document.
    pub raw: toml::Table,
}

// ---- the wire form: everything optional, so that errors name what is missing -----------------------------------

#[derive(Deserialize, Default)]
struct WCompat {
    base: Option<String>,
    base_version: Option<String>,
    arch: Option<String>,
    base_hash: Option<String>,
}
#[derive(Deserialize, Default)]
struct WModalities {
    input: Option<Vec<String>>,
    output: Option<Vec<String>>,
}
#[derive(Deserialize, Default)]
struct WRouting {
    descriptor: Option<String>,
    embed_model: Option<String>,
    centroid: Option<String>,
    keywords: Option<Vec<String>>,
    intents: Option<Vec<String>>,
    prior: Option<f64>,
}
#[derive(Deserialize, Default)]
struct WResources {
    ram_bytes: Option<u64>,
    vram_bytes: Option<u64>,
    load_ms: Option<u64>,
}
#[derive(Deserialize, Default)]
struct WQuality {
    eval_set: Option<String>,
    eval_hash: Option<String>,
    scores: Option<BTreeMap<String, f64>>,
}
#[derive(Deserialize, Default)]
struct WProvenance {
    created_by: Option<String>,
    training_run: Option<String>,
    teachers: Option<Vec<String>>,
    data: Option<Vec<String>>,
}
#[derive(Deserialize, Default)]
struct WPayload {
    path: Option<String>,
    role: Option<String>,
    format: Option<String>,
    bytes: Option<u64>,
    sha256: Option<String>,
    layer: Option<u32>,
    slots: Option<Vec<u32>>,
}
#[derive(Deserialize, Default)]
struct WExperts {
    layers: Option<Vec<u32>>,
    slots_per_layer: Option<u32>,
    hidden: Option<u32>,
    ffn: Option<u32>,
    gate: Option<String>,
}
#[derive(Deserialize, Default)]
struct WMemory {
    chunks: Option<String>,
    embeddings: Option<String>,
    dim: Option<usize>,
    index: Option<String>,
}
#[derive(Deserialize, Default)]
struct WSpecialist {
    engine: Option<String>,
    model: Option<String>,
    tool_description: Option<String>,
}
#[derive(Deserialize, Default)]
struct WProjector {
    from: Option<String>,
    feature_dim: Option<u32>,
    tokens: Option<u32>,
}
#[derive(Deserialize, Default)]
struct WBundle {
    members: Option<Vec<String>>,
}
#[derive(Deserialize, Default)]
struct WSkill {
    instructions: Option<String>,
    examples: Option<String>,
    tools: Option<Vec<String>>,
}
#[derive(Deserialize, Default)]
struct WTool {
    provider: Option<String>,
    name: Option<String>,
    description: Option<String>,
    examples: Option<String>,
    side_effects: Option<String>,
}
#[derive(Deserialize, Default)]
struct Wire {
    spec: Option<String>,
    id: Option<String>,
    version: Option<String>,
    uuid: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    description: Option<String>,
    domain: Option<String>,
    tags: Option<Vec<String>>,
    license: Option<String>,
    created: Option<toml::value::Datetime>,
    parent: Option<String>,
    requires: Option<Vec<String>>,
    conflicts: Option<Vec<String>>,
    compat: Option<WCompat>,
    modalities: Option<WModalities>,
    routing: Option<WRouting>,
    resources: Option<WResources>,
    quality: Option<WQuality>,
    provenance: Option<WProvenance>,
    payload: Option<Vec<WPayload>>,
    experts: Option<WExperts>,
    memory: Option<WMemory>,
    specialist: Option<WSpecialist>,
    projector: Option<WProjector>,
    bundle: Option<WBundle>,
    skill: Option<WSkill>,
    tool: Option<WTool>,
}

const TOP: &[&str] = &[
    "spec",
    "id",
    "version",
    "uuid",
    "kind",
    "name",
    "description",
    "domain",
    "tags",
    "license",
    "created",
    "parent",
    "requires",
    "conflicts",
    "compat",
    "modalities",
    "routing",
    "resources",
    "quality",
    "provenance",
    "payload",
    "experts",
    "memory",
    "specialist",
    "projector",
    "bundle",
    "skill",
    "tool",
    "consolidation",
    "tag_info",
];
const TABLES: &[(&str, &[&str])] = &[
    ("compat", &["base", "base_version", "arch", "base_hash"]),
    ("modalities", &["input", "output"]),
    (
        "routing",
        &["descriptor", "embed_model", "centroid", "keywords", "intents", "prior"],
    ),
    ("resources", &["ram_bytes", "vram_bytes", "load_ms"]),
    ("quality", &["eval_set", "eval_hash", "scores"]),
    ("provenance", &["created_by", "training_run", "teachers", "data"]),
    ("experts", &["layers", "slots_per_layer", "hidden", "ffn", "gate"]),
    ("memory", &["chunks", "embeddings", "dim", "index"]),
    (
        "specialist",
        &["engine", "model", "input_schema", "output_schema", "tool_description"],
    ),
    ("projector", &["from", "feature_dim", "tokens"]),
    ("bundle", &["members"]),
    ("skill", &["instructions", "examples", "tools"]),
];
const PAYLOAD_KEYS: &[&str] = &["path", "role", "format", "bytes", "sha256", "layer", "slots"];

fn unknown_keys(raw: &toml::Table) -> Vec<String> {
    let mut out = vec![];
    for (k, v) in raw {
        if !TOP.contains(&k.as_str()) {
            out.push(k.clone());
            continue;
        }
        if k == "consolidation" || k == "tag_info" || k == "tool" {
            continue; // checked on their own (a tool table accepts nothing else)
        }
        if let (Some((_, known)), Some(t)) = (TABLES.iter().find(|(n, _)| n == k), v.as_table()) {
            out.extend(
                t.keys()
                    .filter(|x| !known.contains(&x.as_str()))
                    .map(|x| format!("{k}.{x}")),
            );
        }
        if k == "payload" {
            for (i, p) in v.as_array().into_iter().flatten().enumerate() {
                if let Some(t) = p.as_table() {
                    out.extend(
                        t.keys()
                            .filter(|x| !PAYLOAD_KEYS.contains(&x.as_str()))
                            .map(|x| format!("payload[{i}].{x}")),
                    );
                }
            }
        }
    }
    out
}

// ---- syntax rules ------------------------------------------------------------------------------------------------

/// `[a-z0-9]+(\.[a-z0-9-]+)*`, at most 96 characters.
pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 96 {
        return false;
    }
    let mut parts = id.split('.');
    let first = parts.next().unwrap_or("");
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if first.is_empty() || !first.chars().all(alnum) {
        return false;
    }
    parts.all(|p| !p.is_empty() && p.chars().all(|c| alnum(c) || c == '-'))
}

/// A `/`-separated taxonomy path, most general first.
pub fn valid_domain(d: &str) -> bool {
    d.is_empty()
        || d.split('/').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        })
}

fn valid_tag(t: &str) -> bool {
    !t.is_empty() && t.len() <= 64 && t.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `id@range` (a bare id means any version). Returns the id and the range.
pub fn parse_ref(s: &str) -> Result<(String, VersionReq), String> {
    let (id, range) = match s.split_once('@') {
        Some((i, r)) => (i.trim(), r.trim()),
        None => (s.trim(), "*"),
    };
    if !valid_id(id) {
        return Err(format!("{s:?}: {id:?} is not a valid KM id"));
    }
    let req = VersionReq::parse(if range.is_empty() { "*" } else { range })
        .map_err(|e| format!("{s:?}: {range:?} is not a version range ({e})"))?;
    Ok((id.to_string(), req))
}

/// `id@version` (an exact version).
pub fn parse_key(s: &str) -> Result<(String, Version), String> {
    let (id, v) = s
        .split_once('@')
        .ok_or_else(|| format!("{s:?} must look like id@version"))?;
    if !valid_id(id) {
        return Err(format!("{s:?}: {id:?} is not a valid KM id"));
    }
    let v = Version::parse(v).map_err(|e| format!("{s:?}: {v:?} is not a semantic version ({e})"))?;
    Ok((id.to_string(), v))
}

fn parse_one_consolidation(t: &toml::Table, at: &str, bad: &mut Vec<String>) -> Option<Consolidation> {
    let state = t
        .get("state")
        .and_then(toml::Value::as_str)
        .unwrap_or("none")
        .to_string();
    if !matches!(state.as_str(), "none" | "learning" | "absorbed") {
        bad.push(format!("{at}.state must be none, learning or absorbed"));
        return None;
    }
    let uplift = match t.get("uplift") {
        None => None,
        Some(v) => match v.as_float().or_else(|| v.as_integer().map(|i| i as f64)) {
            Some(f) if f.is_finite() => Some(f),
            _ => {
                bad.push(format!("{at}.uplift must be a number"));
                None
            }
        },
    };
    let steps = match t.get("steps") {
        None => 0,
        Some(v) => match v.as_integer().filter(|i| *i >= 0) {
            Some(i) => i as u64,
            None => {
                bad.push(format!("{at}.steps must be a whole number >= 0"));
                0
            }
        },
    };
    Some(Consolidation {
        state,
        uplift,
        last_step: t.get("last_step").map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string(),
        }),
        steps,
    })
}

/// `[consolidation]` applies to the KM's own base; `[consolidation.<core>]` tables are per core.
fn parse_consolidation(raw: &toml::Table, base: &str, bad: &mut Vec<String>) -> BTreeMap<String, Consolidation> {
    let mut out = BTreeMap::new();
    let Some(t) = raw.get("consolidation") else { return out };
    let Some(t) = t.as_table() else {
        bad.push("consolidation must be a table".into());
        return out;
    };
    if t.contains_key("state") || t.is_empty() {
        if let Some(c) = parse_one_consolidation(t, "consolidation", bad) {
            out.insert(base.to_string(), c);
        }
    }
    for (core, v) in t.iter().filter(|(_, v)| v.is_table()) {
        if !matches!(core.as_str(), "dusty" | "misty" | "any") {
            bad.push(format!("consolidation.{core}: the core must be dusty, misty or any"));
            continue;
        }
        if let Some(c) = v
            .as_table()
            .and_then(|x| parse_one_consolidation(x, &format!("consolidation.{core}"), bad))
        {
            out.insert(core.clone(), c);
        }
    }
    out
}

/// `[tag_info]`: `"lang:rust" = { origin = "classifier", confidence = 0.9 }`.
fn parse_tag_meta(raw: &toml::Table, tags: &[String], bad: &mut Vec<String>) -> BTreeMap<String, TagMeta> {
    let mut out: BTreeMap<String, TagMeta> = tags
        .iter()
        .map(|t| {
            (
                t.clone(),
                TagMeta {
                    origin: "manual".into(),
                    confidence: 1.0,
                },
            )
        })
        .collect();
    let Some(info) = raw.get("tag_info") else { return out };
    let Some(info) = info.as_table() else {
        bad.push("tag_info must be a table".into());
        return out;
    };
    for (tag, v) in info {
        if !tags.contains(tag) {
            bad.push(format!("tag_info has {tag:?}, which is not in tags"));
            continue;
        }
        let Some(t) = v.as_table() else {
            bad.push(format!("tag_info.{tag} must be {{ origin, confidence }}"));
            continue;
        };
        let origin = t
            .get("origin")
            .and_then(toml::Value::as_str)
            .unwrap_or("manual")
            .to_string();
        if !["manual", "owner", "classifier", "nearest", "author_model", "import"].contains(&origin.as_str()) {
            bad.push(format!(
                "tag_info.{tag}.origin {origin:?} must be manual, owner, classifier, nearest, author_model or import"
            ));
        }
        let conf = t
            .get("confidence")
            .and_then(|c| c.as_float().or_else(|| c.as_integer().map(|i| i as f64)))
            .unwrap_or(1.0);
        if !(0.0..=1.0).contains(&conf) {
            bad.push(format!("tag_info.{tag}.confidence must be between 0 and 1"));
        }
        out.insert(
            tag.clone(),
            TagMeta {
                origin,
                confidence: conf,
            },
        );
    }
    out
}

fn invalid(problems: Vec<String>) -> Fail {
    Fail::invalid(format!("invalid km.toml: {}", problems.join("; ")))
}

impl Manifest {
    pub fn key(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    pub fn total_bytes(&self) -> u64 {
        self.payload.iter().map(|p| p.bytes).sum()
    }

    pub fn payload_of(&self, role: &str) -> Option<&Payload> {
        self.payload.iter().find(|p| p.role == role)
    }

    /// Files a KM directory or archive may hold besides km.toml (km-spec.md §9: nothing the manifest does not name).
    pub fn allowed_files(&self) -> BTreeSet<String> {
        let mut s: BTreeSet<String> = ["km.toml", "README.md", "SIGNATURE"].map(String::from).into();
        s.extend(self.payload.iter().map(|p| p.path.clone()));
        s.extend(self.quality.eval_set.clone());
        s
    }

    pub fn to_json(&self) -> Value {
        let mut v = serde_json::to_value(&self.raw).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("content_id".into(), json!(self.content_id));
        }
        v
    }

    pub fn parse(text: &str) -> Res<Manifest> {
        let raw: toml::Table = toml::from_str(text).map_err(|e| Fail::invalid(format!("invalid km.toml: {e}")))?;
        let w: Wire = toml::from_str(text).map_err(|e| Fail::invalid(format!("invalid km.toml: {e}")))?;
        let mut bad: Vec<String> = vec![];
        let mut warnings = vec![];

        match w.spec.as_deref() {
            Some(SPEC) => {}
            Some(other) => bad.push(format!("spec is {other:?}, this runtime reads {SPEC:?}")),
            None => bad.push(format!("spec is required (spec = \"{SPEC}\")")),
        }
        let id = match w.id.as_deref() {
            Some(i) if valid_id(i) => i.to_string(),
            Some(i) => {
                bad.push(format!(
                    "id {i:?} must be a lowercase dotted slug ([a-z0-9]+(.[a-z0-9-]+)*, at most 96 characters)"
                ));
                String::new()
            }
            None => {
                bad.push("id is required".into());
                String::new()
            }
        };
        let version = match w.version.as_deref().map(Version::parse) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                bad.push(format!(
                    "version {:?} is not a semantic version ({e})",
                    w.version.unwrap_or_default()
                ));
                Version::new(0, 0, 0)
            }
            None => {
                bad.push("version is required".into());
                Version::new(0, 0, 0)
            }
        };
        let kind = match w.kind.as_deref() {
            Some(k) => match Kind::parse(k) {
                Some(k) => k,
                None => {
                    bad.push(format!(
                        "kind {k:?} is unknown (one of {})",
                        Kind::ALL.map(Kind::as_str).join(", ")
                    ));
                    Kind::Data
                }
            },
            None => {
                bad.push("kind is required".into());
                Kind::Data
            }
        };
        let name = match w.name.as_deref().map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => {
                bad.push("name is required".into());
                String::new()
            }
        };
        let domain = w.domain.clone().unwrap_or_default();
        if !valid_domain(&domain) {
            bad.push(format!(
                "domain {domain:?} must be '/'-separated segments of [a-z0-9._-], most general first"
            ));
        }
        let tags = w.tags.clone().unwrap_or_default();
        for t in &tags {
            if !valid_tag(t) {
                bad.push(format!("tag {t:?} is empty, too long (64) or has whitespace"));
            }
        }
        if let Some(p) = &w.parent {
            if let Err(e) = parse_key(p) {
                bad.push(format!("parent: {e}"));
            }
        }
        let requires = w.requires.clone().unwrap_or_default();
        let conflicts = w.conflicts.clone().unwrap_or_default();
        for (what, list) in [("requires", &requires), ("conflicts", &conflicts)] {
            for r in list {
                if let Err(e) = parse_ref(r) {
                    bad.push(format!("{what}: {e}"));
                }
            }
        }
        if (requires.iter().chain(&conflicts)).any(|r| parse_ref(r).is_ok_and(|(i, _)| i == id)) {
            bad.push("a KM cannot require or conflict with itself".into());
        }

        let wc = w.compat.unwrap_or_default();
        let compat = Compat {
            base: wc.base.unwrap_or_else(|| "any".into()),
            base_version: wc.base_version,
            arch: wc.arch,
            base_hash: wc.base_hash,
        };
        if !matches!(compat.base.as_str(), "dusty" | "misty" | "any") {
            bad.push(format!("compat.base {:?} must be dusty, misty or any", compat.base));
        }
        if let Some(bv) = &compat.base_version {
            if let Err(e) = VersionReq::parse(bv) {
                bad.push(format!("compat.base_version {bv:?} is not a version range ({e})"));
            }
        }
        if let Some(h) = &compat.base_hash {
            if !h.strip_prefix("sha256:").is_some_and(valid_sha256) {
                bad.push("compat.base_hash must be sha256:<64 hex digits>".into());
            }
        }
        if kind.needs_base() {
            if compat.base == "any" {
                bad.push(format!(
                    "a {} KM is bound to one base: compat.base must be dusty or misty",
                    kind.as_str()
                ));
            }
            if compat.base_hash.is_none() {
                warnings.push(format!(
                    "compat.base_hash is not set: this {} cannot be checked against the exact base weights",
                    kind.as_str()
                ));
            }
        }

        let wm = w.modalities.unwrap_or_default();
        let input = wm.input.unwrap_or_else(|| vec!["text".into()]);
        let output = wm.output.unwrap_or_else(|| vec!["text".into()]);
        if input.iter().chain(&output).any(|m| m.trim().is_empty()) {
            bad.push("modalities must not contain empty names".into());
        }

        let wr = w.routing.unwrap_or_default();
        let routing = Routing {
            descriptor: wr.descriptor.unwrap_or_default(),
            embed_model: wr.embed_model,
            centroid: wr.centroid,
            keywords: wr.keywords.unwrap_or_default(),
            intents: wr.intents.unwrap_or_default(),
            prior: wr.prior.unwrap_or(0.0),
        };
        if !routing.prior.is_finite() {
            bad.push("routing.prior must be a finite number".into());
        }
        if routing.centroid.is_some() && routing.embed_model.as_deref().is_none_or(str::is_empty) {
            bad.push("routing.centroid needs routing.embed_model (the embedder that made it)".into());
        }

        let wres = w.resources.unwrap_or_default();
        let resources = Resources {
            ram_bytes: wres.ram_bytes,
            vram_bytes: wres.vram_bytes,
            load_ms: wres.load_ms,
        };
        let wq = w.quality.unwrap_or_default();
        let quality = Quality {
            eval_set: wq.eval_set,
            eval_hash: wq.eval_hash,
            scores: wq.scores.unwrap_or_default(),
        };
        if let Some(h) = &quality.eval_hash {
            if !h
                .strip_prefix("sha256:")
                .unwrap_or(h)
                .chars()
                .all(|c| c.is_ascii_hexdigit())
                || h.len() < 64
            {
                bad.push("quality.eval_hash must be sha256:<64 hex digits>".into());
            }
        }
        let wp = w.provenance.unwrap_or_default();
        let provenance = Provenance {
            created_by: wp.created_by.unwrap_or_else(|| "manual".into()),
            training_run: wp.training_run,
            teachers: wp.teachers.unwrap_or_default(),
            data: wp.data.unwrap_or_default(),
        };
        if !matches!(provenance.created_by.as_str(), "manual" | "auto" | "distill" | "import") {
            bad.push(format!(
                "provenance.created_by {:?} must be manual, auto, distill or import",
                provenance.created_by
            ));
        }

        // payload
        let mut payload = vec![];
        let mut seen = BTreeSet::new();
        for (i, p) in w.payload.unwrap_or_default().into_iter().enumerate() {
            let at = format!("payload[{i}]");
            let path = p.path.clone().unwrap_or_default();
            if path.is_empty() {
                bad.push(format!("{at}.path is required"));
                continue;
            }
            if let Err(e) = super::pack::safe_rel(&path) {
                bad.push(format!("{at}.path {path:?}: {e}"));
            }
            if !path.starts_with("payload/") {
                bad.push(format!("{at}.path {path:?} must be under payload/"));
            }
            if !seen.insert(path.to_lowercase()) {
                bad.push(format!("{at}.path {path:?} is listed twice"));
            }
            let sha = p.sha256.clone().unwrap_or_default();
            if !valid_sha256(&sha) {
                bad.push(format!("{at}.sha256 must be 64 lowercase hex digits"));
            }
            let Some(bytes) = p.bytes else {
                bad.push(format!("{at}.bytes is required"));
                continue;
            };
            let slots = match p.slots.as_deref() {
                None => None,
                Some([a, b]) if a <= b => Some([*a, *b]),
                Some(_) => {
                    bad.push(format!("{at}.slots must be [first, last]"));
                    None
                }
            };
            payload.push(Payload {
                path,
                role: p.role.unwrap_or_else(|| "data".into()),
                format: p.format.unwrap_or_default(),
                bytes,
                sha256: sha,
                layer: p.layer,
                slots,
            });
        }
        let total: u64 = payload.iter().map(|p| p.bytes).fold(0u64, |a, b| a.saturating_add(b));
        if total > kind.max_bytes() {
            bad.push(format!(
                "payload is {total} bytes; the limit for a {} KM is {}",
                kind.as_str(),
                kind.max_bytes()
            ));
        }
        let has_payload = |path: &str| payload.iter().any(|p| p.path == path);
        if let Some(c) = &routing.centroid {
            if !has_payload(c) {
                bad.push(format!("routing.centroid {c:?} is not listed in [[payload]]"));
            }
        }
        if let Some(e) = &quality.eval_set {
            if let Err(m) = super::pack::safe_rel(e) {
                bad.push(format!("quality.eval_set {e:?}: {m}"));
            }
        }

        // kind-specific tables
        let mut experts = None;
        let mut memory = None;
        let mut specialist = None;
        let mut projector = None;
        let mut skill = None;
        let mut tool = None;
        let mut members = vec![];
        match kind {
            Kind::Adapter | Kind::Router => {
                if payload.is_empty() {
                    bad.push(format!("a {} KM needs at least one [[payload]] file", kind.as_str()));
                }
            }
            Kind::Experts => match w.experts {
                None => bad.push("an experts KM needs an [experts] table".into()),
                Some(e) => {
                    let layers = e.layers.unwrap_or_default();
                    if layers.is_empty() {
                        bad.push("experts.layers must list at least one layer".into());
                    }
                    let spl = e.slots_per_layer.unwrap_or(0);
                    if spl == 0 {
                        bad.push("experts.slots_per_layer must be > 0".into());
                    }
                    if payload.is_empty() {
                        bad.push("an experts KM needs [[payload]] files".into());
                    }
                    for (i, p) in payload.iter().enumerate() {
                        if routing.centroid.as_deref() == Some(p.path.as_str()) {
                            continue; // the selector's centroid belongs to no layer
                        }
                        match (p.layer, p.slots) {
                            (Some(l), Some([_, last])) => {
                                if !layers.contains(&l) {
                                    bad.push(format!("payload[{i}].layer {l} is not in experts.layers"));
                                }
                                if spl > 0 && last >= spl {
                                    bad.push(format!("payload[{i}].slots end at {last}, past slots_per_layer {spl}"));
                                }
                            }
                            _ => bad.push(format!("payload[{i}] must name its layer and slots = [first, last]")),
                        }
                    }
                    experts = Some(ExpertsT {
                        layers,
                        slots_per_layer: spl,
                        hidden: e.hidden,
                        ffn: e.ffn,
                        gate: e.gate.unwrap_or_else(|| "swiglu".into()),
                    });
                }
            },
            Kind::Memory => match w.memory {
                None => bad.push("a memory KM needs a [memory] table".into()),
                Some(m) => {
                    let chunks = m.chunks.unwrap_or_default();
                    let embeddings = m.embeddings.unwrap_or_default();
                    for (what, p) in [("chunks", &chunks), ("embeddings", &embeddings)] {
                        if p.is_empty() {
                            bad.push(format!("memory.{what} is required"));
                        } else if !has_payload(p) {
                            bad.push(format!("memory.{what} {p:?} is not listed in [[payload]]"));
                        }
                    }
                    let dim = m.dim.unwrap_or(0);
                    if dim == 0 {
                        bad.push("memory.dim must be > 0".into());
                    }
                    let index = m.index.unwrap_or_else(|| "flat".into());
                    if !matches!(index.as_str(), "flat" | "hnsw") {
                        bad.push("memory.index must be flat or hnsw".into());
                    }
                    if routing.embed_model.as_deref().is_none_or(str::is_empty) {
                        bad.push("a memory KM needs routing.embed_model (its chunks are embedded with it)".into());
                    }
                    memory = Some(MemoryT {
                        chunks,
                        embeddings,
                        dim,
                        index,
                    });
                }
            },
            Kind::Data => {
                if !payload.iter().any(|p| p.role == "records") {
                    bad.push("a data KM needs a [[payload]] with role = \"records\" (JSONL, km-spec.md §8)".into());
                }
            }
            Kind::Specialist => match w.specialist {
                None => bad.push("a specialist KM needs a [specialist] table".into()),
                Some(s) => {
                    let engine = s.engine.unwrap_or_default();
                    if !matches!(engine.as_str(), "tts" | "image" | "stt" | "vision") {
                        bad.push("specialist.engine must be tts, image, stt or vision".into());
                    }
                    let model = s.model.unwrap_or_default();
                    if model.trim().is_empty() {
                        bad.push("specialist.model (a model the hub serves) is required".into());
                    }
                    specialist = Some(SpecialistT {
                        engine,
                        model,
                        tool_description: s.tool_description,
                    });
                }
            },
            Kind::Projector => match w.projector {
                None => bad.push("a projector KM needs a [projector] table".into()),
                Some(p) => {
                    let from = p.from.unwrap_or_default();
                    if !valid_id(&from) {
                        bad.push("projector.from must be a specialist KM id".into());
                    }
                    let (fd, tk) = (p.feature_dim.unwrap_or(0), p.tokens.unwrap_or(0));
                    if fd == 0 || tk == 0 {
                        bad.push("projector.feature_dim and projector.tokens must be > 0".into());
                    }
                    if payload.is_empty() {
                        bad.push("a projector KM needs [[payload]] files".into());
                    }
                    projector = Some(ProjectorT {
                        from,
                        feature_dim: fd,
                        tokens: tk,
                    });
                }
            },
            Kind::Skill => match w.skill {
                None => bad.push("a skill KM needs a [skill] table".into()),
                Some(sk) => {
                    let instructions = sk.instructions.unwrap_or_default();
                    if instructions.is_empty() || !has_payload(&instructions) {
                        bad.push("skill.instructions must name a file listed in [[payload]]".into());
                    }
                    if let Some(e) = &sk.examples {
                        if let Err(m) = parse_ref(e) {
                            bad.push(format!("skill.examples: {m}"));
                        }
                    }
                    let tools = sk.tools.unwrap_or_default();
                    for t in &tools {
                        if !valid_id(t) {
                            bad.push(format!("skill.tools: {t:?} is not a tool KM id"));
                        }
                    }
                    skill = Some(SkillT {
                        instructions,
                        examples: sk.examples,
                        tools,
                    });
                }
            },
            Kind::Tool => {
                const ALLOWED: [&str; 7] = [
                    "provider",
                    "name",
                    "input_schema",
                    "output_schema",
                    "description",
                    "examples",
                    "side_effects",
                ];
                if let Some(t) = raw.get("tool").and_then(toml::Value::as_table) {
                    for k in t.keys().filter(|k| !ALLOWED.contains(&k.as_str())) {
                        bad.push(format!(
                            "tool.{k} is not allowed: a tool KM only refers to a tool the host provides; it grants no \
                             permission and carries no command, address or credential"
                        ));
                    }
                }
                for k in [
                    "permissions",
                    "grants",
                    "grant",
                    "allow",
                    "scopes",
                    "capabilities",
                    "command",
                    "exec",
                ] {
                    if raw.contains_key(k) {
                        bad.push(format!("{k} is not allowed in a tool KM (it can grant nothing)"));
                    }
                }
                for p in &payload {
                    let ext = p.path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                    if routing.centroid.as_deref() == Some(p.path.as_str()) {
                        continue; // numbers for the selector, not executable
                    }
                    if !["json", "jsonl", "md", "txt"].contains(&ext.as_str()) {
                        bad.push(format!("{}: a tool KM holds text and JSON only, not .{ext}", p.path));
                    }
                }
                match w.tool {
                    None => bad.push("a tool KM needs a [tool] table".into()),
                    Some(t) => {
                        let provider = t.provider.unwrap_or_default();
                        let name = t.name.unwrap_or_default();
                        let server_ok = |s: &str| {
                            !s.is_empty()
                                && s.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                        };
                        match provider.strip_prefix("mcp:") {
                            Some(srv) if server_ok(srv) => {}
                            None if provider == "hub" => {}
                            _ => bad.push("tool.provider must be \"hub\" or \"mcp:<server>\"".into()),
                        }
                        if name.is_empty()
                            || !name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                        {
                            bad.push(
                                "tool.name must be the name of a hub operation or MCP tool ([A-Za-z0-9._-])".into(),
                            );
                        }
                        let side = t.side_effects.unwrap_or_else(|| "none".into());
                        if !matches!(side.as_str(), "none" | "read" | "write") {
                            bad.push("tool.side_effects must be none, read or write".into());
                        }
                        if let Some(e) = &t.examples {
                            if !has_payload(e) {
                                bad.push(format!("tool.examples {e:?} is not listed in [[payload]]"));
                            }
                        }
                        tool = Some(ToolT {
                            provider,
                            name,
                            description: t.description.unwrap_or_default(),
                            examples: t.examples,
                            side_effects: side,
                        });
                    }
                }
            }
            Kind::Bundle => {
                members = w.bundle.and_then(|b| b.members).unwrap_or_default();
                if members.is_empty() {
                    bad.push("a bundle KM needs bundle.members = [\"id@range\", ...]".into());
                }
                for m in &members {
                    match parse_ref(m) {
                        Ok((i, _)) if i == id => bad.push("a bundle cannot contain itself".into()),
                        Ok(_) => {}
                        Err(e) => bad.push(format!("bundle.members: {e}")),
                    }
                }
            }
        }
        for t in [
            "experts",
            "memory",
            "specialist",
            "projector",
            "bundle",
            "skill",
            "tool",
        ] {
            if raw.contains_key(t) && t != kind.as_str() && !(t == "experts" && kind == Kind::Experts) {
                warnings.push(format!("[{t}] is ignored by a {} KM", kind.as_str()));
            }
        }

        let consolidation = parse_consolidation(&raw, &compat.base, &mut bad);
        let tag_meta = parse_tag_meta(&raw, &tags, &mut bad);

        if !bad.is_empty() {
            return Err(invalid(bad));
        }
        Ok(Manifest {
            id,
            version,
            uuid: w.uuid,
            kind,
            name,
            description: w.description.unwrap_or_default(),
            domain,
            tags,
            license: w.license,
            created: w.created.map(|d| d.to_string()),
            parent: w.parent,
            requires,
            conflicts,
            compat,
            input,
            output,
            routing,
            resources,
            quality,
            provenance,
            payload,
            experts,
            memory,
            specialist,
            projector,
            members,
            skill,
            tool,
            consolidation,
            tag_meta,
            content_id: sha256_hex(text.as_bytes()),
            unknown: unknown_keys(&raw),
            warnings,
            raw,
        })
    }

    pub fn from_file(path: &std::path::Path) -> Res<Manifest> {
        let bytes = std::fs::read(path).map_err(|e| Fail::invalid(format!("{}: {e}", path.display())))?;
        if bytes.len() > 1 << 20 {
            return Err(Fail::invalid("km.toml is over 1 MiB"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| Fail::invalid("km.toml is not UTF-8"))?;
        Manifest::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
spec = "mmkm/1"
id = "code.rust.async"
version = "1.2.0"
kind = "adapter"
name = "Rust async"
description = "Writing async Rust."
domain = "code/rust/async"
tags = ["task:codegen", "lang:rust"]
created = 2026-10-10T12:00:00Z
future_key = "kept"

[compat]
base = "dusty"
base_version = ">=1.0, <2.0"
arch = "qwen35moe"

[routing]
descriptor = "async rust tokio"
embed_model = "hash-384"
prior = 0.5
surprise = 1

[[payload]]
path = "payload/adapter.gguf"
role = "lora"
format = "gguf"
bytes = 10
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#;

    fn with(extra_replace: &[(&str, &str)]) -> Res<Manifest> {
        let mut t = BASE.to_string();
        for (a, b) in extra_replace {
            assert!(t.contains(a), "{a}");
            t = t.replace(a, b);
        }
        Manifest::parse(&t)
    }

    #[test]
    fn a_full_manifest_parses_and_keeps_unknown_keys() {
        let m = Manifest::parse(BASE).unwrap();
        assert_eq!(m.key(), "code.rust.async@1.2.0");
        assert_eq!(m.kind, Kind::Adapter);
        assert_eq!(m.compat.base, "dusty");
        assert_eq!(m.routing.prior, 0.5);
        assert_eq!(m.content_id, sha256_hex(BASE.as_bytes()));
        assert!(m.unknown.contains(&"future_key".to_string()));
        assert!(m.unknown.contains(&"routing.surprise".to_string()));
        assert_eq!(m.raw["future_key"].as_str(), Some("kept"));
        assert!(m.created.as_deref().unwrap().starts_with("2026-10-10"));
        assert!(m.warnings.iter().any(|w| w.contains("base_hash")));
        // defaults
        assert_eq!(m.input, vec!["text"]);
        assert_eq!(m.provenance.created_by, "manual");
    }

    #[test]
    fn the_content_id_is_the_hash_of_the_bytes() {
        let a = Manifest::parse(BASE).unwrap();
        let b = Manifest::parse(&format!("{BASE}\n# a comment\n")).unwrap();
        assert_ne!(a.content_id, b.content_id);
        assert_eq!(a.content_id.len(), 64);
    }

    #[test]
    fn ids_follow_the_slug_rule() {
        for ok in ["a", "code.rust", "code.rust-async.v2", "a1.b-2"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in [
            "",
            "Code",
            "code..rust",
            ".code",
            "code.",
            "code_rust",
            "-a",
            "a.-",
            "a b",
        ] {
            if bad == "a.-" {
                assert!(valid_id(bad)); // '-' is allowed after the first dot
            } else {
                assert!(!valid_id(bad), "{bad}");
            }
        }
        assert!(!valid_id(&"a".repeat(97)));
        assert!(valid_id(&"a".repeat(96)));
        let e = with(&[("id = \"code.rust.async\"", "id = \"Code.Rust\"")]).unwrap_err();
        assert!(e.message.contains("lowercase dotted slug"), "{}", e.message);
    }

    #[test]
    fn versions_must_be_semver_and_spec_must_match() {
        let e = with(&[("version = \"1.2.0\"", "version = \"1.2\"")]).unwrap_err();
        assert!(e.message.contains("semantic version"), "{}", e.message);
        let e = with(&[("spec = \"mmkm/1\"", "spec = \"mmkm/2\"")]).unwrap_err();
        assert!(e.message.contains("mmkm/1"), "{}", e.message);
        assert!(with(&[("version = \"1.2.0\"", "version = \"2.0.0-rc.1+b7\"")]).is_ok());
    }

    #[test]
    fn problems_are_all_reported_at_once() {
        let e = Manifest::parse("spec = \"mmkm/1\"\nkind = \"nope\"\n").unwrap_err();
        for w in [
            "id is required",
            "version is required",
            "kind \"nope\"",
            "name is required",
        ] {
            assert!(e.message.contains(w), "{w}: {}", e.message);
        }
    }

    #[test]
    fn base_bound_kinds_need_a_base() {
        let e = with(&[("base = \"dusty\"", "base = \"any\"")]).unwrap_err();
        assert!(
            e.message.contains("compat.base must be dusty or misty"),
            "{}",
            e.message
        );
        let e = with(&[("base = \"dusty\"", "base = \"llama\"")]).unwrap_err();
        assert!(e.message.contains("dusty, misty or any"), "{}", e.message);
        let e = with(&[(">=1.0, <2.0", "bigger than one")]).unwrap_err();
        assert!(e.message.contains("base_version"), "{}", e.message);
    }

    #[test]
    fn payload_rules() {
        let e = with(&[("payload/adapter.gguf", "../adapter.gguf")]).unwrap_err();
        assert!(e.message.contains("payload[0].path"), "{}", e.message);
        let e = with(&[("payload/adapter.gguf", "other/adapter.gguf")]).unwrap_err();
        assert!(e.message.contains("under payload/"), "{}", e.message);
        let e = with(&[("sha256 = \"0000", "sha256 = \"ZZ00")]).unwrap_err();
        assert!(e.message.contains("sha256"), "{}", e.message);
        let dup = format!(
            "{BASE}\n[[payload]]\npath = \"payload/ADAPTER.gguf\"\nbytes = 1\nsha256 = \"{}\"\n",
            "0".repeat(64)
        );
        assert!(Manifest::parse(&dup).unwrap_err().message.contains("twice"));
        let no_payload = BASE.split("[[payload]]").next().unwrap();
        assert!(Manifest::parse(no_payload)
            .unwrap_err()
            .message
            .contains("at least one"));
    }

    #[test]
    fn requires_and_conflicts_are_checked_for_syntax() {
        let m = with(&[(
            "future_key = \"kept\"",
            "requires = [\"base.skill@^1.0\", \"other\"]\nconflicts = [\"bad@<2\"]",
        )])
        .unwrap();
        assert_eq!(m.requires.len(), 2);
        let e = with(&[("future_key = \"kept\"", "requires = [\"x@not a range\"]")]).unwrap_err();
        assert!(e.message.contains("requires"), "{}", e.message);
        let e = with(&[("future_key = \"kept\"", "requires = [\"code.rust.async@1\"]")]).unwrap_err();
        assert!(e.message.contains("itself"), "{}", e.message);
    }

    #[test]
    fn parse_ref_and_key() {
        let (id, req) = parse_ref("a.b@>=1.0, <2.0").unwrap();
        assert_eq!(id, "a.b");
        assert!(req.matches(&Version::new(1, 5, 0)) && !req.matches(&Version::new(2, 0, 0)));
        assert!(parse_ref("a.b").unwrap().1.matches(&Version::new(9, 0, 0)));
        assert!(parse_ref("A@1").is_err());
        assert_eq!(parse_key("a@1.2.3").unwrap().1, Version::new(1, 2, 3));
        assert!(parse_key("a").is_err());
    }

    #[test]
    fn kind_tables_are_required() {
        let mem = BASE
            .replace("kind = \"adapter\"", "kind = \"memory\"")
            .replace("base = \"dusty\"", "base = \"any\"");
        assert!(Manifest::parse(&mem).unwrap_err().message.contains("[memory] table"));
        let ex = BASE.replace("kind = \"adapter\"", "kind = \"experts\"");
        assert!(Manifest::parse(&ex).unwrap_err().message.contains("[experts] table"));
        let sp = BASE.replace("kind = \"adapter\"", "kind = \"specialist\"");
        assert!(Manifest::parse(&sp).unwrap_err().message.contains("[specialist] table"));
        let b = BASE.replace("kind = \"adapter\"", "kind = \"bundle\"");
        assert!(Manifest::parse(&b).unwrap_err().message.contains("bundle.members"));
        let d = BASE.replace("kind = \"adapter\"", "kind = \"data\"");
        assert!(Manifest::parse(&d).unwrap_err().message.contains("records"));
        let p = BASE.replace("kind = \"adapter\"", "kind = \"projector\"");
        assert!(Manifest::parse(&p).unwrap_err().message.contains("[projector] table"));
    }

    #[test]
    fn an_oversized_kind_is_refused() {
        let huge = BASE.replace("bytes = 10", &format!("bytes = {}", Kind::Adapter.max_bytes() + 1));
        assert!(Manifest::parse(&huge).unwrap_err().message.contains("limit"));
    }

    #[test]
    fn a_centroid_needs_its_embedder_and_a_payload_entry() {
        let c = BASE.replace("prior = 0.5", "prior = 0.5\ncentroid = \"payload/centroid.f16\"");
        assert!(Manifest::parse(&c).unwrap_err().message.contains("not listed"));
        let c = c.replace("embed_model = \"hash-384\"\n", "");
        assert!(Manifest::parse(&c).unwrap_err().message.contains("embed_model"));
    }
}

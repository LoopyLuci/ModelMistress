//! Tiny KMs of every kind, built programmatically, for tests and for trying the runtime without real models.
//!
//! The "weights" are filler bytes (they are not valid GGUF), so these load through the bookkeeping and the verification
//! but would not run in llama.cpp. Centroids come from [`HashEmbedder`], so their `embed_model` is `hash-384`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::embed::{f32_to_f16, HashEmbedder};
use super::sha256_hex;

/// Builds one KM folder `<root>/<id>@<version>/`.
#[derive(Clone)]
pub struct Builder {
    root: PathBuf,
    kind: &'static str,
    id: String,
    version: String,
    name: String,
    description: String,
    domain: String,
    tags: Vec<String>,
    descriptor: String,
    keywords: Vec<String>,
    embed_model: Option<String>,
    centroid_of: Option<String>,
    centroid_vec: Option<Vec<f32>>,
    base: String,
    requires: Vec<String>,
    conflicts: Vec<String>,
    prior: f64,
    ram_bytes: Option<u64>,
    vram_bytes: Option<u64>,
    /// (path, role, format, bytes)
    files: Vec<(String, String, String, Vec<u8>)>,
    tables: String,
    top: String,
    parent: Option<String>,
    input: Vec<String>,
}

impl Builder {
    pub fn new(root: &Path, kind: &'static str, id: &str, version: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            kind,
            id: id.into(),
            version: version.into(),
            name: id.into(),
            description: format!("Sample {kind} KM {id}."),
            domain: String::new(),
            tags: vec![],
            descriptor: String::new(),
            keywords: vec![],
            embed_model: None,
            centroid_of: None,
            centroid_vec: None,
            base: if matches!(kind, "adapter" | "experts" | "projector" | "router") {
                "dusty".into()
            } else {
                "any".into()
            },
            requires: vec![],
            conflicts: vec![],
            prior: 0.0,
            ram_bytes: None,
            vram_bytes: None,
            files: vec![],
            tables: String::new(),
            top: String::new(),
            parent: None,
            input: vec!["text".into()],
        }
    }
    pub fn name(mut self, v: &str) -> Self {
        self.name = v.into();
        self
    }
    pub fn description(mut self, v: &str) -> Self {
        self.description = v.into();
        self
    }
    pub fn domain(mut self, v: &str) -> Self {
        self.domain = v.into();
        self
    }
    pub fn tags(mut self, v: &[&str]) -> Self {
        self.tags = v.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn keywords(mut self, v: &[&str]) -> Self {
        self.keywords = v.iter().map(|s| s.to_string()).collect();
        self
    }
    /// The descriptor text; unless `no_centroid` is called, the centroid is the hash embedding of it.
    pub fn descriptor(mut self, v: &str) -> Self {
        self.descriptor = v.into();
        self.centroid_of = Some(v.into());
        self.embed_model = Some(HashEmbedder::default().dim_id());
        self
    }
    pub fn embed_model(mut self, v: &str) -> Self {
        self.embed_model = Some(v.into());
        self
    }
    /// A centroid of the given values (made by the embedder `embed_model` names) instead of the hash embedding.
    pub fn centroid_vec(mut self, v: &[f32]) -> Self {
        self.centroid_vec = Some(v.to_vec());
        self.centroid_of = Some(String::new());
        self
    }
    pub fn no_centroid(mut self) -> Self {
        self.centroid_of = None;
        self
    }
    pub fn base(mut self, v: &str) -> Self {
        self.base = v.into();
        self
    }
    pub fn requires(mut self, v: &[&str]) -> Self {
        self.requires = v.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn conflicts(mut self, v: &[&str]) -> Self {
        self.conflicts = v.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn prior(mut self, v: f64) -> Self {
        self.prior = v;
        self
    }
    pub fn declared(mut self, ram: u64, vram: u64) -> Self {
        self.ram_bytes = Some(ram);
        self.vram_bytes = Some(vram);
        self
    }
    pub fn parent(mut self, v: &str) -> Self {
        self.parent = Some(v.into());
        self
    }
    pub fn input(mut self, v: &[&str]) -> Self {
        self.input = v.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn file(mut self, path: &str, role: &str, format: &str, bytes: Vec<u8>) -> Self {
        self.files.push((path.into(), role.into(), format.into(), bytes));
        self
    }
    /// Raw TOML appended after the generated tables (a kind table, or extra keys).
    pub fn tables(mut self, toml: &str) -> Self {
        self.tables.push_str(toml);
        self.tables.push('\n');
        self
    }
    /// TOML placed before the first table (top-level keys).
    pub fn top(mut self, toml: &str) -> Self {
        self.top.push_str(toml);
        self.top.push('\n');
        self
    }

    pub fn build(mut self) -> PathBuf {
        let dir = self.root.join(format!("{}@{}", self.id, self.version));
        std::fs::create_dir_all(dir.join("payload")).expect("a folder");
        let embedder = HashEmbedder::default();
        if let Some(text) = self.centroid_of.clone() {
            let c = f32_to_f16(&self.centroid_vec.clone().unwrap_or_else(|| embedder.embed_one(&text)));
            self.files
                .push(("payload/centroid.f16".into(), "centroid".into(), "f16".into(), c));
        }
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let list = |v: &[String]| format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "));
        let mut t = String::new();
        t.push_str(&format!(
            "spec = \"mmkm/1\"\nid = {}\nversion = {}\nkind = \"{}\"\nname = {}\ndescription = {}\ndomain = {}\ntags = {}\n",
            q(&self.id),
            q(&self.version),
            self.kind,
            q(&self.name),
            q(&self.description),
            q(&self.domain),
            list(&self.tags)
        ));
        t.push_str("license = \"Apache-2.0\"\ncreated = 2026-10-10T12:00:00Z\n");
        if let Some(p) = &self.parent {
            t.push_str(&format!("parent = {}\n", q(p)));
        }
        if !self.requires.is_empty() {
            t.push_str(&format!("requires = {}\n", list(&self.requires)));
        }
        if !self.conflicts.is_empty() {
            t.push_str(&format!("conflicts = {}\n", list(&self.conflicts)));
        }
        t.push_str(&self.top);
        t.push_str(&format!("\n[compat]\nbase = {}\n", q(&self.base)));
        if self.base != "any" {
            t.push_str(&format!(
                "base_version = \">=1.0, <2.0\"\narch = \"qwen35moe\"\nbase_hash = \"sha256:{}\"\n",
                sha256_hex(self.base.as_bytes())
            ));
        }
        t.push_str(&format!(
            "\n[modalities]\ninput = {}\noutput = [\"text\"]\n",
            list(&self.input)
        ));
        t.push_str(&format!(
            "\n[routing]\ndescriptor = {}\nkeywords = {}\nprior = {}\n",
            q(&self.descriptor),
            list(&self.keywords),
            self.prior
        ));
        if let Some(m) = &self.embed_model {
            t.push_str(&format!("embed_model = {}\n", q(m)));
        }
        if self.centroid_of.is_some() {
            t.push_str("centroid = \"payload/centroid.f16\"\n");
        }
        if self.ram_bytes.is_some() || self.vram_bytes.is_some() {
            t.push_str(&format!(
                "\n[resources]\nram_bytes = {}\nvram_bytes = {}\n",
                self.ram_bytes.unwrap_or(0),
                self.vram_bytes.unwrap_or(0)
            ));
        }
        t.push_str("\n[provenance]\ncreated_by = \"manual\"\n");
        t.push_str(&self.tables);
        for (path, role, format, bytes) in &self.files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().expect("a parent")).expect("a folder");
            std::fs::write(&full, bytes).expect("a file");
            t.push_str(&format!(
                "\n[[payload]]\npath = {}\nrole = {}\nformat = {}\nbytes = {}\nsha256 = \"{}\"\n",
                q(path),
                q(role),
                q(format),
                bytes.len(),
                sha256_hex(bytes)
            ));
        }
        std::fs::write(dir.join("km.toml"), t).expect("km.toml");
        dir
    }
}

impl HashEmbedder {
    pub fn dim_id(&self) -> String {
        use super::embed::Embedder;
        self.model_id()
    }
}

/// Filler bytes: deterministic, `n` long.
pub fn filler(seed: &str, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = sha256_hex(seed.as_bytes()).into_bytes();
    while out.len() < n {
        out.extend_from_slice(&h);
        h = sha256_hex(&h).into_bytes();
    }
    out.truncate(n);
    out
}

pub fn adapter(root: &Path, id: &str, version: &str, descriptor: &str) -> PathBuf {
    adapter_sized(root, id, version, descriptor, 2048)
}

pub fn adapter_sized(root: &Path, id: &str, version: &str, descriptor: &str, bytes: usize) -> PathBuf {
    Builder::new(root, "adapter", id, version)
        .descriptor(descriptor)
        .domain("code/rust")
        .tags(&["lang:rust", "task:codegen"])
        .file(
            "payload/adapter.gguf",
            "lora",
            "gguf",
            filler(&format!("{id}@{version}"), bytes),
        )
        .build()
}

pub fn experts(root: &Path, id: &str, version: &str, descriptor: &str) -> PathBuf {
    Builder::new(root, "experts", id, version)
        .descriptor(descriptor)
        .tables("[experts]\nlayers = [4, 5]\nslots_per_layer = 8\nhidden = 64\nffn = 128\ngate = \"swiglu\"")
        .file("payload/l4.gguf", "experts", "gguf", filler("l4", 1024))
        .file("payload/l5.gguf", "experts", "gguf", filler("l5", 1024))
        .build_experts()
}

/// An experts KM whose payloads are the given GGUF fragments `(layer, [first slot, last slot], file bytes)`, for
/// assembly into a real (or synthetic) core. Dimensions are the core's `hidden` and `ffn`.
pub fn experts_with(
    root: &Path,
    id: &str,
    version: &str,
    (hidden, ffn, slots_per_layer): (u32, u32, u32),
    parts: &[(u32, [u32; 2], Vec<u8>)],
) -> PathBuf {
    let mut layers: Vec<u32> = parts.iter().map(|p| p.0).collect();
    layers.sort_unstable();
    layers.dedup();
    let mut b = Builder::new(root, "experts", id, version).descriptor(id).tables(&format!(
        "[experts]\nlayers = {layers:?}\nslots_per_layer = {slots_per_layer}\nhidden = {hidden}\nffn = {ffn}\ngate = \"swiglu\""
    ));
    for (layer, [a, z], bytes) in parts {
        b = b.file(
            &format!("payload/l{layer}-{a}-{z}.gguf"),
            "experts",
            "gguf",
            bytes.clone(),
        );
    }
    let dir = b.build();
    let p = dir.join("km.toml");
    let mut t = std::fs::read_to_string(&p).expect("km.toml");
    for (layer, [a, z], _) in parts {
        let path = format!("path = \"payload/l{layer}-{a}-{z}.gguf\"");
        t = t.replace(&path, &format!("{path}\nlayer = {layer}\nslots = [{a}, {z}]"));
    }
    std::fs::write(p, t).expect("km.toml");
    dir
}

impl Builder {
    /// Experts payload files name their layer and slots; patch them into the generated manifest.
    fn build_experts(self) -> PathBuf {
        let dir = self.build();
        let p = dir.join("km.toml");
        let t = std::fs::read_to_string(&p).expect("km.toml");
        let t = t
            .replace(
                "path = \"payload/l4.gguf\"",
                "path = \"payload/l4.gguf\"\nlayer = 4\nslots = [0, 3]",
            )
            .replace(
                "path = \"payload/l5.gguf\"",
                "path = \"payload/l5.gguf\"\nlayer = 5\nslots = [0, 7]",
            );
        std::fs::write(p, t).expect("km.toml");
        dir
    }
}

/// A memory KM from `(chunk id, text)` pairs, embedded with the hash embedder.
pub fn memory(root: &Path, id: &str, version: &str, chunks: &[(&str, &str)]) -> PathBuf {
    let e = HashEmbedder::default();
    let mut jsonl = String::new();
    let mut emb: Vec<u8> = vec![];
    for (cid, text) in chunks {
        jsonl.push_str(&json!({"id": cid, "text": text, "source": format!("doc-{cid}")}).to_string());
        jsonl.push('\n');
        emb.extend(f32_to_f16(&e.embed_one(text)));
    }
    let descriptor = chunks.iter().map(|c| c.1).collect::<Vec<_>>().join(" ");
    Builder::new(root, "memory", id, version)
        .descriptor(&descriptor)
        .domain("docs")
        .tags(&["modality:text"])
        .tables(&format!(
            "[memory]\nchunks = \"payload/chunks.jsonl\"\nembeddings = \"payload/emb.f16\"\ndim = {}\nindex = \"flat\"",
            e.dim
        ))
        .file("payload/chunks.jsonl", "chunks", "jsonl", jsonl.into_bytes())
        .file("payload/emb.f16", "embeddings", "f16", emb)
        .build()
}

/// A data KM from record objects (`id`, `input`, `output`, ...).
pub fn data(root: &Path, id: &str, version: &str, records: &[Value]) -> PathBuf {
    let jsonl: String = records.iter().map(|r| format!("{r}\n")).collect();
    Builder::new(root, "data", id, version)
        .descriptor("training records")
        .domain("data/sample")
        .file("payload/records.jsonl", "records", "jsonl", jsonl.into_bytes())
        .build()
}

pub fn record(id: &str, user: &str, answer: &str, status: &str) -> Value {
    json!({"id": id, "task": "chat", "input": [{"role": "user", "content": user}], "output": answer,
           "labels": {"quality": 0.9}, "tags": ["task:codegen"], "source": {"kind": "teacher", "model": "dusty@1.0.0"},
           "split": "train", "status": status, "rev": 1})
}

pub fn specialist(root: &Path, id: &str, version: &str, engine: &str, model: &str) -> PathBuf {
    Builder::new(root, "specialist", id, version)
        .descriptor(&format!("{engine} specialist"))
        .tables(&format!(
            "[specialist]\nengine = \"{engine}\"\nmodel = \"{model}\"\ntool_description = \"calls {model}\""
        ))
        .build()
}

pub fn projector(root: &Path, id: &str, version: &str, from: &str) -> PathBuf {
    Builder::new(root, "projector", id, version)
        .descriptor("vision projector")
        .input(&["image", "text"])
        .tables(&format!(
            "[projector]\nfrom = \"{from}\"\nfeature_dim = 512\ntokens = 16"
        ))
        .file("payload/proj.gguf", "projector", "gguf", filler(id, 1024))
        .build()
}

pub fn router(root: &Path, id: &str, version: &str) -> PathBuf {
    Builder::new(root, "router", id, version)
        .descriptor("router")
        .file("payload/router.gguf", "router", "gguf", filler(id, 512))
        .build()
}

pub fn bundle(root: &Path, id: &str, version: &str, members: &[&str]) -> PathBuf {
    let list = members
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Builder::new(root, "bundle", id, version)
        .descriptor("a bundle")
        .tables(&format!("[bundle]\nmembers = [{list}]"))
        .build()
}

/// A skill KM: instructions as context, examples from a data KM, tool references.
pub fn skill(root: &Path, id: &str, version: &str, examples: Option<&str>, tools: &[&str]) -> PathBuf {
    let tool_list = tools.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
    let ex = examples.map(|e| format!("examples = \"{e}\"\n")).unwrap_or_default();
    Builder::new(root, "skill", id, version)
        .descriptor("a skill with instructions")
        .tables(&format!(
            "[skill]\ninstructions = \"payload/SKILL.md\"\n{ex}tools = [{tool_list}]"
        ))
        .file(
            "payload/SKILL.md",
            "instructions",
            "md",
            b"# Skill\nDo the thing carefully.\n".to_vec(),
        )
        .build()
}

/// A tool KM that refers to a hub operation or an MCP tool.
pub fn tool(root: &Path, id: &str, version: &str, provider: &str, name: &str) -> PathBuf {
    Builder::new(root, "tool", id, version)
        .descriptor(&format!("tool {name}"))
        .tables(&format!(
            "[tool]\nprovider = \"{provider}\"\nname = \"{name}\"\ndescription = \"calls {name}\"\nside_effects = \"read\"\nexamples = \"payload/examples.jsonl\""
        ))
        .file("payload/examples.jsonl", "examples", "jsonl", b"{\"call\":{}}\n".to_vec())
        .build()
}

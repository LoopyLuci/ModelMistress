//! The model catalog: every GGUF model on this machine that ModelMistress can serve.
//!
//! Three kinds of places are scanned:
//! - Ollama stores (`<dir>/manifests/<host>/<namespace>/<model>/<tag>` pointing at `<dir>/blobs/sha256-...`).
//!   The id is Ollama's own name, `llama3.2:1b`, or `namespace/model:tag` outside the library.
//! - Hugging Face hub caches (`<dir>/models--<org>--<name>/snapshots/<rev>/...gguf`), which is where Unsloth
//!   keeps its downloads. The id is `org/name:file-stem`.
//! - Plain folders of `.gguf` files (searched 4 levels deep). The id is the file stem.
//!
//! `mmproj*.gguf` files (vision projectors) are not models; they are attached to the model beside them.
//! Only the first part of a split model (`-00001-of-0000N.gguf`) is listed; llama.cpp loads the rest.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub source: &'static str,
    pub path: PathBuf,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mmproj: Option<PathBuf>,
    /// Scalar metadata from the GGUF header: `general.*` and `<architecture>.*`.
    pub meta: Map<String, Value>,
}

impl ModelEntry {
    pub fn architecture(&self) -> Option<&str> {
        self.meta.get("general.architecture").and_then(Value::as_str)
    }

    pub fn context_length(&self) -> Option<u64> {
        let arch = self.architecture()?;
        self.meta.get(&format!("{arch}.context_length")).and_then(Value::as_u64)
    }

    /// A short summary for listings.
    pub fn summary(&self, loaded: bool) -> Value {
        json!({
            "id": self.id,
            "source": self.source,
            "size_gb": (self.size_bytes as f64 / 1e9 * 100.0).round() / 100.0,
            "architecture": self.architecture(),
            "name": self.meta.get("general.name"),
            "size_label": self.meta.get("general.size_label"),
            "file_type": self.meta.get("general.file_type").and_then(Value::as_u64).map(file_type_name),
            "context_length": self.context_length(),
            "vision": self.mmproj.is_some(),
            "loaded": loaded,
        })
    }
}

/// Where to look. Missing folders are skipped quietly.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    pub ollama: Vec<PathBuf>,
    pub huggingface: Vec<PathBuf>,
    pub folders: Vec<PathBuf>,
}

impl Roots {
    /// The usual places on this machine, from the environment.
    pub fn detected() -> Self {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        let mut r = Roots::default();
        if let Some(p) = std::env::var_os("OLLAMA_MODELS") {
            r.ollama.push(PathBuf::from(p));
        }
        if let Some(h) = &home {
            r.ollama.push(h.join(".ollama").join("models"));
        }
        if cfg!(target_os = "linux") {
            r.ollama.push(PathBuf::from("/usr/share/ollama/.ollama/models"));
            r.ollama.push(PathBuf::from("/var/lib/ollama/models"));
        }
        if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
            r.huggingface.push(PathBuf::from(p));
        }
        if let Some(p) = std::env::var_os("HF_HOME") {
            r.huggingface.push(PathBuf::from(p).join("hub"));
        }
        if let Some(h) = &home {
            r.huggingface.push(h.join(".cache").join("huggingface").join("hub"));
            r.folders.push(h.join(".cache").join("lm-studio").join("models"));
        }
        r
    }
}

pub fn scan(roots: &Roots) -> Vec<ModelEntry> {
    let mut found: BTreeMap<String, ModelEntry> = BTreeMap::new();
    let mut add = |e: ModelEntry| {
        found.entry(e.id.clone()).or_insert(e);
    };
    for dir in dedup(&roots.ollama) {
        scan_ollama(&dir, &mut add);
    }
    for dir in dedup(&roots.huggingface) {
        scan_hf(&dir, &mut add);
    }
    for dir in dedup(&roots.folders) {
        let mut files = Vec::new();
        walk(&dir, 4, &mut files);
        for f in files {
            if is_model_file(&f) {
                let stem = model_stem(&f);
                let mmproj = sibling_mmproj(&f);
                if let Some(e) = entry(stem, "folder", f, mmproj) {
                    add(e);
                }
            }
        }
    }
    found.into_values().collect()
}

fn dedup(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = Vec::<PathBuf>::new();
    for d in dirs {
        let c = plain(d.canonicalize().unwrap_or_else(|_| d.clone()));
        if c.is_dir() && !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen
}

/// Windows' canonical paths start with `\\?\`; drop it (for a drive path) so logs and other programs read them plainly.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

fn entry(id: String, source: &'static str, path: PathBuf, mmproj: Option<PathBuf>) -> Option<ModelEntry> {
    let size_bytes = std::fs::metadata(&path).ok()?.len();
    let meta = read_meta(&path).ok()?; // not a readable GGUF: not a model we can serve
    Some(ModelEntry {
        id,
        source,
        path,
        size_bytes,
        mmproj,
        meta,
    })
}

fn scan_ollama(dir: &Path, add: &mut impl FnMut(ModelEntry)) {
    let manifests = dir.join("manifests");
    let mut files = Vec::new();
    walk(&manifests, 4, &mut files);
    for f in files {
        let Ok(rel) = f.strip_prefix(&manifests) else { continue };
        let parts: Vec<String> = rel.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let [host, ns, model, tag] = parts.as_slice() else {
            continue;
        };
        let Ok(doc) = std::fs::read(&f).map(|b| serde_json::from_slice::<Value>(&b)) else {
            continue;
        };
        let Ok(doc) = doc else { continue };
        let blob = |media: &str| {
            doc["layers"].as_array().and_then(|ls| {
                ls.iter()
                    .find(|l| l["mediaType"] == media)
                    .and_then(|l| l["digest"].as_str())
                    .map(|d| dir.join("blobs").join(d.replace(':', "-")))
                    .filter(|p| p.is_file())
            })
        };
        let Some(path) = blob("application/vnd.ollama.image.model") else {
            continue;
        }; // e.g. a cloud model
        let id = if host == "registry.ollama.ai" && ns == "library" {
            format!("{model}:{tag}")
        } else {
            format!("{ns}/{model}:{tag}")
        };
        if let Some(e) = entry(id, "ollama", path, blob("application/vnd.ollama.image.projector")) {
            add(e);
        }
    }
}

fn scan_hf(dir: &Path, add: &mut impl FnMut(ModelEntry)) {
    let Ok(repos) = std::fs::read_dir(dir) else { return };
    for repo in repos.flatten() {
        let name = repo.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("models--") else {
            continue;
        };
        let repo_id = rest.replacen("--", "/", 1);
        let Ok(snaps) = std::fs::read_dir(repo.path().join("snapshots")) else {
            continue;
        };
        for snap in snaps.flatten() {
            let mut files = Vec::new();
            walk(&snap.path(), 3, &mut files);
            let mmproj = files.iter().find(|f| is_mmproj(f)).cloned();
            for f in files.into_iter().filter(|f| is_model_file(f)) {
                let id = format!("{repo_id}:{}", model_stem(&f));
                if let Some(e) = entry(id, "huggingface", f, mmproj.clone()) {
                    add(e);
                }
            }
        }
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        // metadata() follows links (the HF cache is made of them); file_type() would not.
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() && depth > 1 => walk(&p, depth - 1, out),
            Ok(m) if m.is_file() => out.push(p),
            _ => {}
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_mmproj(p: &Path) -> bool {
    let n = file_name(p);
    n.ends_with(".gguf") && n.starts_with("mmproj")
}

/// A `.gguf` that is a model: not a projector, and the first part if it is split.
fn is_model_file(p: &Path) -> bool {
    let n = file_name(p);
    if !n.ends_with(".gguf") || is_mmproj(p) {
        return false;
    }
    match split_part(&n) {
        Some((part, _)) => part == 1,
        None => true,
    }
}

/// `name-00002-of-00003.gguf` -> (2, 3)
fn split_part(name: &str) -> Option<(u32, u32)> {
    let stem = name.strip_suffix(".gguf")?;
    let (head, total) = stem.rsplit_once("-of-")?;
    let (_, part) = head.rsplit_once('-')?;
    Some((part.parse().ok()?, total.parse().ok()?))
}

fn model_stem(p: &Path) -> String {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.rsplit_once("-of-") {
        Some((head, _)) if split_part(&file_name(p)).is_some() => {
            head.rsplit_once('-').map(|(h, _)| h.to_string()).unwrap_or(stem)
        }
        _ => stem,
    }
}

fn sibling_mmproj(p: &Path) -> Option<PathBuf> {
    let dir = p.parent()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|f| is_mmproj(f))
}

/// llama.cpp's `general.file_type` numbers.
pub fn file_type_name(t: u64) -> String {
    let s = match t {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4",
        _ => return format!("type {t}"),
    };
    s.to_string()
}

// ---- GGUF header: just the scalar metadata, without reading tensors or the tokenizer -------------------

const MAX_STRING: u64 = 1 << 20;

pub fn read_meta(path: &Path) -> std::io::Result<Map<String, Value>> {
    let mut r = BufReader::with_capacity(1 << 16, File::open(path)?);
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    if &read_n::<4>(&mut r)? != b"GGUF" {
        return Err(bad("not a GGUF file"));
    }
    let version = u32::from_le_bytes(read_n(&mut r)?);
    if !(2..=3).contains(&version) {
        return Err(bad("unsupported GGUF version"));
    }
    let _tensors = u64::from_le_bytes(read_n(&mut r)?);
    let n_kv = u64::from_le_bytes(read_n(&mut r)?);
    let mut meta = Map::new();
    let mut arch = String::new();
    for _ in 0..n_kv.min(100_000) {
        let key = read_str(&mut r)?;
        // The tokenizer (huge arrays) and everything after it is not needed.
        if key.starts_with("tokenizer.") {
            break;
        }
        let ty = u32::from_le_bytes(read_n(&mut r)?);
        let wanted = key.starts_with("general.") || (!arch.is_empty() && key.starts_with(&format!("{arch}.")));
        match read_value(&mut r, ty)? {
            Some(v) if wanted => {
                if key == "general.architecture" {
                    arch = v.as_str().unwrap_or_default().to_string();
                }
                meta.insert(key, v);
            }
            _ => {}
        }
    }
    Ok(meta)
}

fn read_n<const N: usize>(r: &mut impl Read) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn read_str(r: &mut (impl Read + Seek)) -> std::io::Result<String> {
    let n = u64::from_le_bytes(read_n(r)?);
    if n > MAX_STRING {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "string too long"));
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// Reads one value. Scalars and strings come back; arrays are skipped (None).
fn read_value(r: &mut BufReader<File>, ty: u32) -> std::io::Result<Option<Value>> {
    Ok(Some(match ty {
        0 => json!(read_n::<1>(r)?[0]),
        1 => json!(read_n::<1>(r)?[0] as i8),
        2 => json!(u16::from_le_bytes(read_n(r)?)),
        3 => json!(i16::from_le_bytes(read_n(r)?)),
        4 => json!(u32::from_le_bytes(read_n(r)?)),
        5 => json!(i32::from_le_bytes(read_n(r)?)),
        6 => json!(f32::from_le_bytes(read_n(r)?)),
        7 => json!(read_n::<1>(r)?[0] != 0),
        8 => json!(read_str(r)?),
        9 => {
            let ety = u32::from_le_bytes(read_n(r)?);
            let n = u64::from_le_bytes(read_n(r)?);
            match scalar_size(ety) {
                Some(sz) => r.seek_relative((sz * n) as i64)?,
                None if ety == 8 => {
                    for _ in 0..n {
                        let len = u64::from_le_bytes(read_n(r)?);
                        r.seek_relative(len as i64)?;
                    }
                }
                None => {
                    for _ in 0..n {
                        read_value(r, ety)?;
                    }
                }
            }
            return Ok(None);
        }
        10 => json!(u64::from_le_bytes(read_n(r)?)),
        11 => json!(i64::from_le_bytes(read_n(r)?)),
        12 => json!(f64::from_le_bytes(read_n(r)?)),
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown GGUF value type",
            ))
        }
    }))
}

fn scalar_size(ty: u32) -> Option<u64> {
    match ty {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gguf(path: &Path, arch: &str, name: &str) {
        let mut b = Vec::new();
        b.extend_from_slice(b"GGUF");
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&5u64.to_le_bytes());
        let s = |b: &mut Vec<u8>, v: &str| {
            b.extend_from_slice(&(v.len() as u64).to_le_bytes());
            b.extend_from_slice(v.as_bytes());
        };
        s(&mut b, "general.architecture");
        b.extend_from_slice(&8u32.to_le_bytes());
        s(&mut b, arch);
        s(&mut b, "general.name");
        b.extend_from_slice(&8u32.to_le_bytes());
        s(&mut b, name);
        s(&mut b, "general.tags"); // an array of strings: skipped
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&2u64.to_le_bytes());
        s(&mut b, "a");
        s(&mut b, "bc");
        s(&mut b, &format!("{arch}.context_length"));
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&4096u32.to_le_bytes());
        s(&mut b, "tokenizer.ggml.model"); // reading stops here
        b.extend_from_slice(&8u32.to_le_bytes());
        s(&mut b, "gpt2");
        std::fs::write(path, b).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-catalog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_scalar_metadata_and_skips_arrays() {
        let d = tmp("meta");
        let f = d.join("m.gguf");
        gguf(&f, "llama", "Tiny");
        let m = read_meta(&f).unwrap();
        assert_eq!(m["general.architecture"], "llama");
        assert_eq!(m["general.name"], "Tiny");
        assert_eq!(m["llama.context_length"], 4096);
        assert!(!m.contains_key("general.tags"));
        assert!(!m.contains_key("tokenizer.ggml.model"));
        std::fs::write(d.join("bad.gguf"), b"nope").unwrap();
        assert!(read_meta(&d.join("bad.gguf")).is_err());
    }

    #[test]
    fn scans_ollama_hf_and_folders() {
        let d = tmp("scan");
        // Ollama: one local model, one cloud model with no weights
        let ol = d.join("ollama");
        std::fs::create_dir_all(ol.join("blobs")).unwrap();
        gguf(&ol.join("blobs/sha256-aa"), "llama", "L");
        for (m, t, layers) in [
            (
                "llama3.2",
                "1b",
                r#"[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:aa"}]"#,
            ),
            ("big", "cloud", r#"[]"#),
        ] {
            let p = ol.join("manifests/registry.ollama.ai/library").join(m);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join(t), format!(r#"{{"layers":{layers}}}"#)).unwrap();
        }
        // HF cache with a projector and a split model
        let snap = d.join("hf/models--unsloth--Qwen-GGUF/snapshots/abc");
        std::fs::create_dir_all(&snap).unwrap();
        gguf(&snap.join("Qwen-Q4_K_M.gguf"), "qwen3", "Q");
        gguf(&snap.join("mmproj-F16.gguf"), "clip", "P");
        gguf(&snap.join("Big-00001-of-00002.gguf"), "qwen3", "B");
        gguf(&snap.join("Big-00002-of-00002.gguf"), "qwen3", "B");
        // a plain folder
        std::fs::create_dir_all(d.join("dir/sub")).unwrap();
        gguf(&d.join("dir/sub/mine.gguf"), "phi3", "Mine");
        std::fs::write(d.join("dir/notes.txt"), "x").unwrap();

        let roots = Roots {
            ollama: vec![ol],
            huggingface: vec![d.join("hf")],
            folders: vec![d.join("dir")],
        };
        let ids: Vec<String> = scan(&roots).into_iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [
                "llama3.2:1b",
                "mine",
                "unsloth/Qwen-GGUF:Big",
                "unsloth/Qwen-GGUF:Qwen-Q4_K_M"
            ]
        );
        let all = scan(&roots);
        let q = all.iter().find(|e| e.id.ends_with("Qwen-Q4_K_M")).unwrap();
        assert!(q.mmproj.is_some());
        assert_eq!(q.context_length(), Some(4096));
        assert_eq!(q.summary(false)["vision"], true);
    }

    #[test]
    fn split_names() {
        assert_eq!(split_part("x-00001-of-00003.gguf"), Some((1, 3)));
        assert_eq!(split_part("x.gguf"), None);
        assert_eq!(model_stem(Path::new("a/Big-00001-of-00002.gguf")), "Big");
        assert_eq!(model_stem(Path::new("a/one-of-us.gguf")), "one-of-us");
    }
}

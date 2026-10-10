//! Image generation with stable-diffusion.cpp's `sd-cli` (built from the vendored tree with the `bundled-engine`
//! feature, or found next to `llama-server`, or named by MM_SD_CLI). One request starts one `sd-cli` process: the
//! model is loaded, used and freed, one at a time (the same way text to speech runs). That keeps a model's
//! gigabytes out of memory between requests and makes cancelling a plain kill.
//!
//! A model is a file of a store folder: SD 1.x / SDXL / SD3 checkpoints (`.safetensors`, `.ckpt`, `.gguf`) are used
//! whole; FLUX and friends come as a diffusion model plus a VAE and text encoders, which are paired by name from
//! the model's own folder (see docs/images.md). Nothing here starts a process; the hub does that.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::catalog::ModelEntry;
use crate::engine::{Fail, Result};

/// The most characters a prompt may have (OpenAI's limit for the image models).
pub const MAX_PROMPT_CHARS: usize = 4000;
/// The most images one request may ask for.
pub const MAX_IMAGES: u32 = 10;
/// The smallest and largest side, in pixels.
pub const SIDE_RANGE: std::ops::RangeInclusive<u32> = 64..=2048;
/// The size used when a request gives none.
pub const DEFAULT_SIZE: (u32, u32) = (512, 512);

/// What OpenAI clients send as `model`: it means "the default image model".
const OPENAI_MODELS: &[&str] = &[
    "dall-e-2",
    "dall-e-3",
    "gpt-image-1",
    "gpt-image-1-mini",
    "gpt-image-1.5",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    B64Json,
    Url,
}

impl Format {
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s.to_lowercase().as_str() {
            "b64_json" => Ok(Self::B64Json),
            "url" => Ok(Self::Url),
            other => Err(format!(
                "response_format \"{other}\" is not available: use b64_json or url"
            )),
        }
    }
}

/// "512x768" (or "512X768") as (width, height); "" and "auto" mean the default size.
pub fn parse_size(s: &str) -> std::result::Result<(u32, u32), String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return Ok(DEFAULT_SIZE);
    }
    let bad = || format!("size \"{s}\" must be WIDTHxHEIGHT, for example 512x512");
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(bad)?;
    let (w, h): (u32, u32) = (
        w.trim().parse().map_err(|_| bad())?,
        h.trim().parse().map_err(|_| bad())?,
    );
    for side in [w, h] {
        if !SIDE_RANGE.contains(&side) || side % 8 != 0 {
            return Err(format!(
                "size {w}x{h}: each side must be a multiple of 8 from {} to {}",
                SIDE_RANGE.start(),
                SIDE_RANGE.end()
            ));
        }
    }
    Ok((w, h))
}

/// A parsed `POST /v1/images/generations` body (or the `image.generate` operation's input).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub n: u32,
    pub width: u32,
    pub height: u32,
    pub format: Format,
    /// Sampling steps; unset: the tool's own default (20).
    pub steps: Option<u32>,
    /// Guidance scale; unset: the tool's own default (7; FLUX models want 1).
    pub cfg_scale: Option<f32>,
    pub seed: Option<i64>,
    pub sampler: Option<String>,
}

impl ImageRequest {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        let obj = v.as_object().ok_or("the body must be a JSON object")?;
        let text = |k: &str| -> std::result::Result<Option<String>, String> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
                Some(_) => Err(format!("{k} must be a string")),
            }
        };
        let prompt = text("prompt")?.ok_or("prompt is required")?;
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(format!("prompt is over {MAX_PROMPT_CHARS} characters"));
        }
        let n = match obj.get("n") {
            None | Some(Value::Null) => 1,
            Some(x) => x
                .as_u64()
                .filter(|n| (1..=MAX_IMAGES as u64).contains(n))
                .ok_or(format!("n must be an integer from 1 to {MAX_IMAGES}"))? as u32,
        };
        let (width, height) = parse_size(&text("size")?.unwrap_or_default())?;
        let steps = match obj.get("steps") {
            None | Some(Value::Null) => None,
            Some(x) => Some(
                x.as_u64()
                    .filter(|n| (1..=150).contains(n))
                    .ok_or("steps must be an integer from 1 to 150")? as u32,
            ),
        };
        // OpenAI's `quality` picks the effort; explicit steps win
        let steps = match (steps, text("quality")?.map(|q| q.to_lowercase())) {
            (Some(s), _) => Some(s),
            (None, None) => None,
            (None, Some(q)) => match q.as_str() {
                "auto" => None,
                "low" => Some(10),
                "medium" | "standard" => Some(20),
                "high" | "hd" => Some(30),
                other => {
                    return Err(format!(
                        "quality \"{other}\": use low, medium, high or auto (or give steps)"
                    ))
                }
            },
        };
        let cfg_scale = match obj.get("cfg_scale") {
            None | Some(Value::Null) => None,
            Some(x) => Some(
                x.as_f64()
                    .filter(|c| (0.0..=30.0).contains(c))
                    .ok_or("cfg_scale must be a number from 0 to 30")? as f32,
            ),
        };
        let seed = match obj.get("seed") {
            None | Some(Value::Null) => None,
            Some(x) => Some(x.as_i64().ok_or("seed must be an integer")?),
        };
        let sampler = text("sampler")?;
        if let Some(s) = &sampler {
            if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+') {
                return Err(format!("sampler \"{s}\" is not a sampling method name"));
            }
        }
        Ok(Self {
            model: text("model")?.unwrap_or_default(),
            prompt,
            negative_prompt: text("negative_prompt")?,
            n,
            width,
            height,
            format: Format::parse(&text("response_format")?.unwrap_or_else(|| "b64_json".into()))?,
            steps,
            cfg_scale,
            seed,
            sampler,
        })
    }
}

/// Is this the name an OpenAI client sends for "whatever image model there is"?
pub fn is_generic_model(name: &str) -> bool {
    let n = name.trim();
    n.is_empty() || OPENAI_MODELS.iter().any(|m| m.eq_ignore_ascii_case(n))
}

// ---- models -------------------------------------------------------------------------------------------------

/// An image model and the files that go with it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImageModel {
    pub id: String,
    pub path: PathBuf,
    pub vae: Option<PathBuf>,
    pub clip_l: Option<PathBuf>,
    pub clip_g: Option<PathBuf>,
    pub t5xxl: Option<PathBuf>,
    pub llm: Option<PathBuf>,
}

impl ImageModel {
    /// A model that comes in parts (FLUX, and the models that need a separate text encoder) is passed to sd-cli as
    /// `--diffusion-model`; a whole checkpoint as `-m`.
    pub fn is_split(&self) -> bool {
        self.t5xxl.is_some() || self.llm.is_some() || self.id.to_lowercase().contains("flux")
    }
}

const WEIGHT_EXTENSIONS: &[&str] = &["safetensors", "sft", "ckpt", "gguf"];

/// Which file is a text encoder or VAE (a companion), not a model.
fn companion_kind(file: &str) -> Option<&'static str> {
    let f = file.to_lowercase();
    let stem = f.rsplit_once('.').map_or(f.as_str(), |(s, _)| s);
    if f.contains("mmproj") {
        None
    } else if f.contains("clip_l") || f.contains("clip-l") {
        Some("clip_l")
    } else if f.contains("clip_g") || f.contains("clip-g") {
        Some("clip_g")
    } else if f.contains("t5xxl") {
        Some("t5xxl")
    } else if stem == "ae" || f.contains("vae") {
        Some("vae")
    } else if f.starts_with("qwen") && (f.contains("text_encoder") || f.contains("qwen_3") || f.contains("qwen2")) {
        Some("llm")
    } else {
        None
    }
}

fn is_weights(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| WEIGHT_EXTENSIONS.iter().any(|w| w.eq_ignore_ascii_case(e)))
}

fn stem_of(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

/// Weight files in `dir`, `depth` levels down, sorted by path.
fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, out);
            }
        } else if is_weights(&p) {
            out.push(p);
        }
    }
}

/// Does the file name say it is a diffusion model (so that a plain folder of LLMs does not make several "default"
/// image models)?
fn looks_like_image_model(stem: &str) -> bool {
    let s = stem.to_lowercase();
    [
        "sdxl",
        "sd3",
        "sd1",
        "sd2",
        "sd_",
        "sd-",
        "stable-diffusion",
        "stable_diffusion",
        "diffusion",
        "flux",
    ]
    .iter()
    .any(|k| s.contains(k))
        || s == "sd"
}

/// The files of the model's folder that go with it.
fn pair(id: String, path: PathBuf) -> ImageModel {
    let mut m = ImageModel {
        id,
        path: path.clone(),
        ..Default::default()
    };
    let mut files = Vec::new();
    if let Some(dir) = path.parent() {
        walk(dir, 0, &mut files);
    }
    for f in files {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if f == path {
            continue;
        }
        let slot = match companion_kind(&name) {
            Some("vae") => &mut m.vae,
            Some("clip_l") => &mut m.clip_l,
            Some("clip_g") => &mut m.clip_g,
            Some("t5xxl") => &mut m.t5xxl,
            Some("llm") => &mut m.llm,
            _ => continue,
        };
        slot.get_or_insert(f); // sorted by name: the first one found is used
    }
    m
}

/// The image model for `name` (a model id, a file stem, or the path of a file). `dirs` are the folders to search
/// (the first is the images folder; its files are all image models), `catalog` the models the store knows by id.
/// With no name (or an OpenAI one): the `image_model` setting, else the only image model found.
pub fn find_model(dirs: &[PathBuf], catalog: &[ModelEntry], name: &str, preferred: Option<&str>) -> Result<ImageModel> {
    let asked = if is_generic_model(name) {
        preferred.map(str::trim).filter(|p| !p.is_empty()).unwrap_or("")
    } else {
        name.trim()
    };
    let named = |f: &Path, asked: &str| {
        stem_of(f).eq_ignore_ascii_case(asked) || f.file_name().is_some_and(|n| n.eq_ignore_ascii_case(asked))
    };
    if !asked.is_empty() {
        let as_path = Path::new(asked);
        if as_path.is_absolute() {
            return if as_path.is_file() && is_weights(as_path) {
                Ok(pair(stem_of(as_path), as_path.to_path_buf()))
            } else {
                Err(Fail::not_found(format!("no image model file {asked}")))
            };
        }
        if let Some(e) = catalog.iter().find(|e| e.id.eq_ignore_ascii_case(asked)) {
            return Ok(pair(e.id.clone(), e.path.clone()));
        }
        for d in dirs {
            let mut files = Vec::new();
            walk(d, 3, &mut files);
            if let Some(f) = files.into_iter().find(|f| {
                companion_kind(f.file_name().and_then(|n| n.to_str()).unwrap_or("")).is_none() && named(f, asked)
            }) {
                return Ok(pair(stem_of(&f), f));
            }
        }
        return Err(Fail::not_found(format!(
            "no image model \"{asked}\": put its file in {} (see docs/images.md)",
            dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
        )));
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        let mut files = Vec::new();
        walk(d, 3, &mut files);
        found.extend(files.into_iter().filter(|f| {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            companion_kind(name).is_none() && (i == 0 || looks_like_image_model(&stem_of(f)))
        }));
    }
    found.dedup();
    match found.as_slice() {
        [one] => Ok(pair(stem_of(one), one.clone())),
        [] => Err(Fail::not_found(format!(
            "no image model on this machine: put a Stable Diffusion or FLUX model (.safetensors or .gguf) in {}; see \
             docs/images.md",
            dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
        ))),
        many => Err(Fail::invalid(format!(
            "several image models are here ({}); name one in the request or set image_model",
            many.iter().map(|f| stem_of(f)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

// ---- the tool -----------------------------------------------------------------------------------------------

/// The arguments of `sd-cli` for one request. `out` is a printf pattern (`image_%d.png`): `sd-cli` writes one
/// file per image, numbered from 0. `cpu_only` keeps every part of the model off the GPU.
pub fn tool_args(m: &ImageModel, req: &ImageRequest, seed: i64, out: &Path, cpu_only: bool) -> Vec<String> {
    let path = |p: &Path| p.display().to_string();
    let mut a: Vec<String> = vec![
        if m.is_split() { "--diffusion-model" } else { "-m" }.into(),
        path(&m.path),
    ];
    for (flag, file) in [
        ("--vae", &m.vae),
        ("--clip_l", &m.clip_l),
        ("--clip_g", &m.clip_g),
        ("--t5xxl", &m.t5xxl),
        ("--llm", &m.llm),
    ] {
        if let Some(f) = file {
            a.extend([flag.into(), path(f)]);
        }
    }
    a.extend([
        "-p".into(),
        req.prompt.clone(),
        "-W".into(),
        req.width.to_string(),
        "-H".into(),
        req.height.to_string(),
        "-b".into(),
        req.n.to_string(),
        "-s".into(),
        seed.to_string(),
        "-o".into(),
        path(out),
        "--output-begin-idx".into(),
        "0".into(),
    ]);
    if let Some(n) = &req.negative_prompt {
        a.extend(["-n".into(), n.clone()]);
    }
    if let Some(s) = req.steps {
        a.extend(["--steps".into(), s.to_string()]);
    }
    if let Some(c) = req.cfg_scale {
        a.extend(["--cfg-scale".into(), c.to_string()]);
    }
    if let Some(s) = &req.sampler {
        a.extend(["--sampling-method".into(), s.clone()]);
    }
    if cpu_only {
        a.extend(["--backend".into(), "cpu".into()]);
    }
    a
}

/// `sd-cli` draws its sampling progress as `  |=====>    | 3/20 - 1.52s/it` (redrawn with a carriage return) and
/// its loading progress, which this skips, with `#` in the bar. Returns (step, steps).
pub fn progress(line: &str) -> Option<(u64, u64)> {
    let rest = line.trim_start().strip_prefix('|')?;
    let (bar, rest) = rest.rsplit_once('|')?;
    if bar.contains('#') {
        return None;
    }
    let (done, rest) = rest.trim_start().split_once('/')?;
    let total = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    let (done, total): (u64, u64) = (done.trim().parse().ok()?, total.parse().ok()?);
    (total > 0 && done <= total).then_some((done, total))
}

/// The files `sd-cli` wrote for an `out` pattern like `<dir>/image-x_%d.png`, in order.
pub fn output_files(pattern: &Path, n: u32) -> Vec<PathBuf> {
    let p = pattern.display().to_string();
    (0..n)
        .map(|i| PathBuf::from(p.replacen("%d", &i.to_string(), 1)))
        .filter(|f| f.is_file())
        .collect()
}

/// Standard base64 (RFC 4648, with padding).
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-images-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(d: &Path, name: &str) -> PathBuf {
        let p = d.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn parsing_sizes() {
        assert_eq!(parse_size("512x768").unwrap(), (512, 768));
        assert_eq!(parse_size(" 1024X1024 ").unwrap(), (1024, 1024));
        assert_eq!(parse_size("").unwrap(), DEFAULT_SIZE);
        assert_eq!(parse_size("auto").unwrap(), DEFAULT_SIZE);
        for bad in [
            "512",
            "axb",
            "512x",
            "x512",
            "513x512",
            "32x512",
            "4096x4096",
            "-8x8",
            "512x512x3",
        ] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        assert!(parse_size("513x512").unwrap_err().contains("multiple of 8"));
    }

    #[test]
    fn parsing_requests() {
        let r = ImageRequest::from_json(&json!({"prompt": " a cat "})).unwrap();
        assert_eq!(
            (r.prompt.as_str(), r.n, r.width, r.height, r.format, r.steps, r.seed),
            ("a cat", 1, 512, 512, Format::B64Json, None, None)
        );
        let r = ImageRequest::from_json(&json!({
            "model": "dall-e-3", "prompt": "x", "n": 2, "size": "256x384", "response_format": "URL", "steps": 4,
            "seed": -1, "negative_prompt": "blurry", "cfg_scale": 1.0, "sampler": "dpm++2m"
        }))
        .unwrap();
        assert_eq!(
            (r.n, r.width, r.height, r.format, r.steps, r.seed),
            (2, 256, 384, Format::Url, Some(4), Some(-1))
        );
        assert_eq!(
            (r.negative_prompt.as_deref(), r.cfg_scale, r.sampler.as_deref()),
            (Some("blurry"), Some(1.0), Some("dpm++2m"))
        );
        let q = |v: Value| ImageRequest::from_json(&v).unwrap().steps;
        assert_eq!(q(json!({"prompt": "x", "quality": "low"})), Some(10));
        assert_eq!(q(json!({"prompt": "x", "quality": "HD"})), Some(30));
        assert_eq!(q(json!({"prompt": "x", "quality": "auto"})), None);
        assert_eq!(q(json!({"prompt": "x", "quality": "low", "steps": 7})), Some(7));
        let bad = |v: Value| ImageRequest::from_json(&v).unwrap_err();
        assert!(bad(json!({})).contains("prompt"));
        assert!(bad(json!({"prompt": "  "})).contains("prompt"));
        assert!(bad(json!({"prompt": 3})).contains("string"));
        assert!(bad(json!([1])).contains("object"));
        assert!(bad(json!({"prompt": "x".repeat(MAX_PROMPT_CHARS + 1)})).contains("4000"));
        assert!(bad(json!({"prompt": "x", "n": 0})).contains("n must"));
        assert!(bad(json!({"prompt": "x", "n": 11})).contains("n must"));
        assert!(bad(json!({"prompt": "x", "size": "10x10"})).contains("size"));
        assert!(bad(json!({"prompt": "x", "steps": 0})).contains("steps"));
        assert!(bad(json!({"prompt": "x", "quality": "ultra"})).contains("quality"));
        assert!(bad(json!({"prompt": "x", "response_format": "png"})).contains("b64_json"));
        assert!(bad(json!({"prompt": "x", "seed": "a"})).contains("seed"));
        assert!(bad(json!({"prompt": "x", "cfg_scale": 99})).contains("cfg_scale"));
        assert!(bad(json!({"prompt": "x", "sampler": "euler --vae /etc/x"})).contains("sampler"));
        assert!(is_generic_model("") && is_generic_model("DALL-E-3") && !is_generic_model("sdxl-turbo"));
    }

    #[test]
    fn building_the_arguments() {
        let req = ImageRequest::from_json(&json!({"prompt": "a cat", "n": 2, "size": "256x256", "steps": 4})).unwrap();
        let m = ImageModel {
            id: "sd15".into(),
            path: "m.safetensors".into(),
            ..Default::default()
        };
        let a = tool_args(&m, &req, 7, Path::new("o_%d.png"), false);
        assert_eq!(
            a,
            [
                "-m",
                "m.safetensors",
                "-p",
                "a cat",
                "-W",
                "256",
                "-H",
                "256",
                "-b",
                "2",
                "-s",
                "7",
                "-o",
                "o_%d.png",
                "--output-begin-idx",
                "0",
                "--steps",
                "4"
            ]
        );
        let flux = ImageModel {
            id: "flux1-schnell-q4".into(),
            path: "f.gguf".into(),
            vae: Some("ae.safetensors".into()),
            clip_l: Some("clip_l.safetensors".into()),
            t5xxl: Some("t5.safetensors".into()),
            ..Default::default()
        };
        let mut req = req;
        req.negative_prompt = Some("ugly".into());
        req.cfg_scale = Some(1.0);
        req.sampler = Some("euler".into());
        let a = tool_args(&flux, &req, 1, Path::new("o_%d.png"), true);
        assert_eq!(
            &a[..8],
            [
                "--diffusion-model",
                "f.gguf",
                "--vae",
                "ae.safetensors",
                "--clip_l",
                "clip_l.safetensors",
                "--t5xxl",
                "t5.safetensors"
            ]
        );
        assert_eq!(
            &a[a.len() - 10..],
            [
                "-n",
                "ugly",
                "--steps",
                "4",
                "--cfg-scale",
                "1",
                "--sampling-method",
                "euler",
                "--backend",
                "cpu"
            ]
        );
    }

    #[test]
    fn reading_progress() {
        assert_eq!(
            progress("  |=======>                                         | 3/20 - 1.52s/it"),
            Some((3, 20))
        );
        assert_eq!(
            progress("  |==================================================| 20/20 - 2.10it/s\x1b[K"),
            Some((20, 20))
        );
        assert_eq!(
            progress("  |##########>                                       | 5/291 - 41.20MB/s"),
            None
        );
        assert_eq!(
            progress("[INFO ] stable-diffusion.cpp:123 - sampling using Euler method"),
            None
        );
        assert_eq!(progress("  |   | 0/0 - x"), None);
        assert_eq!(progress("  |=| 9/4 - x"), None);
        assert_eq!(progress("  | |"), None);
    }

    #[test]
    fn finding_models() {
        let root = tmp();
        let images = root.join("images");
        let other = root.join("models");
        touch(&images, "sd15.safetensors");
        touch(&other, "llama3.gguf");
        let none = find_model(&[images.join("nope")], &[], "", None).unwrap_err();
        assert!(
            none.status == 404 && none.message.contains("no image model"),
            "{}",
            none.message
        );
        let dirs = [images.clone(), other.clone()];
        // the only one found, also for an OpenAI name
        assert_eq!(find_model(&dirs, &[], "", None).unwrap().id, "sd15");
        assert_eq!(find_model(&dirs, &[], "dall-e-3", None).unwrap().id, "sd15");
        // a model in a folder of LLMs counts only when its name says so
        touch(&other, "sdxl-turbo-q8_0.gguf");
        let two = find_model(&dirs, &[], "", None).unwrap_err();
        assert!(
            two.status == 400 && two.message.contains("sdxl-turbo-q8_0"),
            "{}",
            two.message
        );
        // a name, a file name, any case, a path; or the setting
        assert_eq!(
            find_model(&dirs, &[], "SDXL-turbo-q8_0", None).unwrap().id,
            "sdxl-turbo-q8_0"
        );
        assert_eq!(find_model(&dirs, &[], "sd15.safetensors", None).unwrap().id, "sd15");
        assert_eq!(find_model(&dirs, &[], "", Some("sd15")).unwrap().id, "sd15");
        assert_eq!(find_model(&dirs, &[], "dall-e-2", Some("sd15")).unwrap().id, "sd15");
        let p = images.join("sd15.safetensors");
        assert_eq!(find_model(&dirs, &[], &p.display().to_string(), None).unwrap().path, p);
        // a missing one is a 404
        assert_eq!(find_model(&dirs, &[], "gone", None).unwrap_err().status, 404);
        assert_eq!(find_model(&dirs, &[], "", Some("gone")).unwrap_err().status, 404);
        // a model the store knows by id
        let e = ModelEntry {
            id: "org/flux:q4".into(),
            source: "huggingface",
            path: other.join("flux-q4.gguf"),
            size_bytes: 1,
            mmproj: None,
            meta: Default::default(),
        };
        assert_eq!(
            find_model(&dirs, &[e], "org/flux:q4", None).unwrap().path,
            other.join("flux-q4.gguf")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pairing_flux_files() {
        let d = tmp();
        let model = touch(&d, "flux1-schnell-Q4_K_S.gguf");
        let vae = touch(&d, "ae.safetensors");
        let clip = touch(&d, "clip_l.safetensors");
        let t5 = touch(&d, "t5xxl_fp16.safetensors");
        touch(&d, "notes.txt");
        let m = find_model(std::slice::from_ref(&d), &[], "flux1-schnell-Q4_K_S", None).unwrap();
        assert_eq!(
            (
                m.vae.clone(),
                m.clip_l.clone(),
                m.t5xxl.clone(),
                m.clip_g.clone(),
                m.llm.clone()
            ),
            (Some(vae), Some(clip), Some(t5), None, None)
        );
        assert!(m.is_split());
        // the encoders are not models, so the folder has one model
        assert_eq!(find_model(std::slice::from_ref(&d), &[], "", None).unwrap().path, model);
        let sd = pair("sd15".into(), touch(&d.join("sd"), "sd15.safetensors"));
        assert!(!sd.is_split() && sd.vae.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn output_files_and_base64() {
        let d = tmp();
        let pattern = d.join("img_%d.png");
        touch(&d, "img_0.png");
        touch(&d, "img_1.png");
        assert_eq!(
            output_files(&pattern, 3),
            vec![d.join("img_0.png"), d.join("img_1.png")]
        );
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        let _ = std::fs::remove_dir_all(&d);
    }
}

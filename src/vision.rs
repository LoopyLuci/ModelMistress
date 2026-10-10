//! Image recognition with ONNX Runtime built into the binary (feature `vision`): classify a picture, find the
//! objects in it, or turn it into an embedding vector. A model is an ONNX file in `<home>/vision`, `<home>/models`
//! or a folder in `model_dirs`; an optional `<name>.json` beside it holds what the file itself cannot say (labels,
//! mean and std, input size, the kind). The kind is otherwise read from the model's outputs (see [`guess_kind`]).
//!
//! Everything but running the session (finding models, reading requests, preparing the pixels, reading the
//! output tensors) is plain code that builds without the feature, so it is tested without a model.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::engine::{Fail, Result};

/// Whether this build can run vision models.
pub const AVAILABLE: bool = cfg!(feature = "vision");

pub const NOT_BUILT: &str = "this build of ModelMistress has no image recognition; rebuild with `--features vision`";

/// The largest picture a request may carry, decoded from base64.
pub const MAX_IMAGE_BYTES: usize = 48 << 20;
/// The most pixels a picture may have (it is decoded to 3 bytes each).
pub const MAX_PIXELS: u64 = 64_000_000;
/// The most predictions or detections one answer holds.
pub const MAX_TOP_K: usize = 1000;
/// Used when a request gives no `top_k`: predictions for a classifier, detections for a detector.
pub const DEFAULT_TOP_K: usize = 5;
pub const DEFAULT_MAX_DETECTIONS: usize = 100;
/// Used when a request gives no `threshold` (detection only: the least score of a box).
pub const DEFAULT_THRESHOLD: f32 = 0.25;
/// Boxes of one class that overlap more than this are one object.
pub const DEFAULT_NMS_IOU: f32 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Classify,
    Detect,
    Embed,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "classify" | "classification" => Some(Kind::Classify),
            "detect" | "detection" => Some(Kind::Detect),
            "embed" | "embedding" | "embeddings" => Some(Kind::Embed),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Classify => "classify",
            Kind::Detect => "detect",
            Kind::Embed => "embed",
        }
    }
}

// ---- finding models ---------------------------------------------------------------------------------------------

/// Name given to the models in the API: the file name without `.onnx`.
pub fn model_name(path: &Path) -> Option<String> {
    let f = path.file_name()?.to_string_lossy().to_string();
    let n = f.len().checked_sub(5)?;
    if !f.is_char_boundary(n) || !f[n..].eq_ignore_ascii_case(".onnx") {
        return None;
    }
    Some(f[..n].to_string()).filter(|s| !s.is_empty())
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() && depth > 1 => walk(&p, depth - 1, out),
            Ok(m) if m.is_file() => {
                if let Some(n) = model_name(&p) {
                    out.push((n, p));
                }
            }
            _ => {}
        }
    }
}

/// Every vision model in `dirs`, as (name, path); the first folder wins when two share a name.
pub fn list_models(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut all = Vec::new();
    for d in dirs {
        walk(d, 3, &mut all);
    }
    let mut seen: Vec<String> = Vec::new();
    all.retain(|(n, _)| {
        let key = n.to_lowercase();
        let fresh = !seen.contains(&key);
        seen.push(key);
        fresh
    });
    all
}

/// The model for `name` (a name from [`list_models`], with or without `.onnx`); with no name, the only model found.
pub fn find_model(dirs: &[PathBuf], name: &str) -> Result<(String, PathBuf)> {
    let all = list_models(dirs);
    let asked = name.trim();
    let asked = asked.strip_suffix(".onnx").unwrap_or(asked);
    if asked.is_empty() {
        return match all.len() {
            1 => Ok(all.into_iter().next().unwrap_or_default()),
            0 => Err(Fail::not_found(format!(
                "no vision model on this machine: put an ONNX model in {} (see docs/vision.md)",
                dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
            ))),
            _ => Err(Fail::invalid(format!(
                "several vision models are here, name one in `model`: {}",
                all.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
            ))),
        };
    }
    all.into_iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(asked))
        .ok_or_else(|| {
            Fail::not_found(format!(
                "no vision model \"{asked}\": put its .onnx file in {} (see docs/vision.md)",
                dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
            ))
        })
}

// ---- the sidecar ------------------------------------------------------------------------------------------------

/// The input size in a sidecar: one number (a square) or `[width, height]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Square(u32),
    Pair([u32; 2]),
}

/// What `<name>.json` may say; every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sidecar {
    /// `classify`, `detect` or `embed`: skips guessing.
    pub kind: Option<String>,
    /// The class names, in the model's order.
    pub labels: Vec<String>,
    /// Per channel (R, G, B), applied to the pixels after they are scaled.
    pub mean: Option<[f32; 3]>,
    pub std: Option<[f32; 3]>,
    /// Multiplies the 0..255 pixels before mean and std (default 1/255; use 1 for models that want 0..255).
    pub scale: Option<f32>,
    /// The input size, for models whose input has a dynamic height or width.
    pub size: Option<Size>,
    /// `stretch` (the whole picture is squeezed to the size), `crop` (short side to the size, centre cut) or
    /// `letterbox` (the whole picture, aspect kept, on grey padding).
    pub resize: Option<String>,
    /// With `crop`: the share of the short side kept (default 0.875 for classifiers, else 1).
    pub crop_pct: Option<f32>,
    /// The model wants blue, green, red.
    pub bgr: bool,
    /// Detection: the output rows carry an objectness score before the class scores (YOLOv5: yes, YOLOv8: no).
    pub objectness: Option<bool>,
    /// Detection: the overlap above which two boxes of one class are one object.
    pub nms_iou: Option<f32>,
    /// Embedding: scale the vector to length 1 (default yes).
    pub normalize: Option<bool>,
}

impl Sidecar {
    pub fn parse(text: &str) -> std::result::Result<Sidecar, String> {
        let s: Sidecar = serde_json::from_str(text).map_err(|e| format!("bad sidecar: {e}"))?;
        if let Some(k) = &s.kind {
            if Kind::parse(k).is_none() {
                return Err(format!("bad sidecar: kind \"{k}\" is not classify, detect or embed"));
            }
        }
        if let Some(sd) = s.std {
            if sd.iter().any(|v| !v.is_finite() || *v == 0.0) {
                return Err("bad sidecar: std must be non-zero numbers".into());
            }
        }
        if s.mean.is_some_and(|m| m.iter().any(|v| !v.is_finite()))
            || s.scale.is_some_and(|v| !v.is_finite() || v == 0.0)
        {
            return Err("bad sidecar: mean and scale must be finite numbers".into());
        }
        if let Some(sz) = s.size {
            let (w, h) = sz.dims();
            if !(SIZE_RANGE).contains(&w) || !(SIZE_RANGE).contains(&h) {
                return Err(format!(
                    "bad sidecar: size must be from {} to {}",
                    SIZE_RANGE.start(),
                    SIZE_RANGE.end()
                ));
            }
        }
        if let Some(r) = &s.resize {
            if !matches!(r.as_str(), "stretch" | "crop" | "letterbox") {
                return Err(format!("bad sidecar: resize \"{r}\" is not stretch, crop or letterbox"));
            }
        }
        if s.crop_pct.is_some_and(|v| !(v > 0.0 && v <= 1.0)) {
            return Err("bad sidecar: crop_pct must be above 0 and at most 1".into());
        }
        if s.nms_iou.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
            return Err("bad sidecar: nms_iou must be from 0 to 1".into());
        }
        Ok(s)
    }

    /// The sidecar of the model file `model` (`x.onnx`): `x.json`, else `x.onnx.json`; none is fine.
    pub fn load(model: &Path) -> Result<Sidecar> {
        for p in [
            model.with_extension("json"),
            PathBuf::from(format!("{}.json", model.display())),
        ] {
            if let Ok(text) = std::fs::read_to_string(&p) {
                return Sidecar::parse(&text)
                    .map_err(|e| Fail::new(500, "model_error", format!("{}: {e}", p.display())));
            }
        }
        Ok(Sidecar::default())
    }

    pub fn forced_kind(&self) -> Option<Kind> {
        self.kind.as_deref().and_then(Kind::parse)
    }
}

/// The smallest and largest input side a sidecar may ask for.
const SIZE_RANGE: std::ops::RangeInclusive<u32> = 16..=4096;

impl Size {
    pub fn dims(self) -> (u32, u32) {
        match self {
            Size::Square(s) => (s, s),
            Size::Pair([w, h]) => (w, h),
        }
    }
}

// ---- requests ---------------------------------------------------------------------------------------------------

/// A parsed `POST /v1/vision/*` body (or the `vision.analyze` operation's input).
#[derive(Debug, Clone, PartialEq)]
pub struct VisionRequest {
    pub model: String,
    /// The encoded picture (PNG or JPEG), decoded from base64.
    pub image: Vec<u8>,
    pub top_k: Option<usize>,
    pub threshold: Option<f32>,
}

impl VisionRequest {
    /// `allow_file` lets the input name a `file` on the hub's machine instead of `image` (the operation does, the
    /// routes, which peers may call, do not).
    pub fn from_json(v: &Value, allow_file: bool) -> std::result::Result<Self, String> {
        let obj = v.as_object().ok_or("the body must be a JSON object")?;
        let text = |k: &str| -> std::result::Result<Option<&str>, String> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim()).filter(|s| !s.is_empty())),
                Some(_) => Err(format!("{k} must be a string")),
            }
        };
        let image = match (text("image")?, text("file")?) {
            (Some(i), _) => decode_image_field(i)?,
            (None, Some(f)) if allow_file => {
                let bytes = std::fs::read(f).map_err(|e| format!("{f}: {e}"))?;
                if bytes.len() > MAX_IMAGE_BYTES {
                    return Err(format!("the image is over {} MB", MAX_IMAGE_BYTES >> 20));
                }
                bytes
            }
            _ => return Err("image is required (base64 or a data URL of a PNG or JPEG)".into()),
        };
        let top_k = match obj.get("top_k") {
            None | Some(Value::Null) => None,
            Some(n) => match n.as_u64() {
                Some(k) if (1..=MAX_TOP_K as u64).contains(&k) => Some(k as usize),
                _ => return Err(format!("top_k must be an integer from 1 to {MAX_TOP_K}")),
            },
        };
        let threshold = match obj.get("threshold") {
            None | Some(Value::Null) => None,
            Some(n) => match n.as_f64() {
                Some(t) if (0.0..=1.0).contains(&t) => Some(t as f32),
                _ => return Err("threshold must be a number from 0 to 1".into()),
            },
        };
        Ok(VisionRequest {
            model: text("model")?.unwrap_or("").to_string(),
            image,
            top_k,
            threshold,
        })
    }
}

fn decode_image_field(s: &str) -> std::result::Result<Vec<u8>, String> {
    let data = match s.strip_prefix("data:") {
        Some(rest) => {
            let (head, body) = rest.split_once(',').ok_or("the data URL has no comma")?;
            if !head.ends_with(";base64") {
                return Err("the data URL must be base64".into());
            }
            body
        }
        None => s,
    };
    let bytes = decode_base64(data)?;
    if bytes.is_empty() {
        return Err("image is empty".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("the image is over {} MB", MAX_IMAGE_BYTES >> 20));
    }
    Ok(bytes)
}

/// Base64 (RFC 4648, standard or URL-safe alphabet, padding optional, white space ignored).
pub fn decode_base64(s: &str) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut pad = false;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                pad = true;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err("image is not valid base64".into()),
        };
        if pad {
            return Err("image is not valid base64".into());
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return Err("image is not valid base64".into());
    }
    Ok(out)
}

// ---- preparing the pixels ---------------------------------------------------------------------------------------

/// How a picture is fitted to the model's input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The whole picture is squeezed to the size.
    Stretch,
    /// The short side is scaled to the size and the centre is cut.
    Crop,
    /// The whole picture is scaled to fit, centred on grey padding (what YOLO models are trained with).
    Letterbox,
}

/// The grey of letterbox padding, 0..255.
const PAD: f32 = 114.0;

/// Where a letterboxed picture sits in the input: (left, top, width, height) in input pixels.
fn letterbox_geom(tw: usize, th: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let r = (tw as f32 / w as f32).min(th as f32 / h as f32);
    let nw = ((w as f32 * r).round() as usize).clamp(1, tw);
    let nh = ((h as f32 * r).round() as usize).clamp(1, th);
    ((tw - nw) / 2, (th - nh) / 2, nw, nh)
}

/// How a picture becomes the model's input tensor.
#[derive(Debug, Clone, PartialEq)]
pub struct Prep {
    pub width: usize,
    pub height: usize,
    pub mean: [f32; 3],
    pub std: [f32; 3],
    pub scale: f32,
    /// How the picture is fitted to the size.
    pub fit: Fit,
    /// With `Fit::Crop`: the share of the short side that is kept (the short side is scaled to size / crop_pct, then
    /// the centre is cut), 0.875 for ImageNet classifiers.
    pub crop_pct: f32,
    pub bgr: bool,
    /// Channels last (`[1, h, w, 3]`) instead of `[1, 3, h, w]`.
    pub nhwc: bool,
}

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];
const CLIP_MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const CLIP_STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

impl Prep {
    /// The usual preparation for `kind` (ImageNet statistics and a 0.875 centre crop for classifiers, CLIP's statistics
    /// for embedders, plain 0..1 and a letterbox for detectors), then the sidecar's changes.
    pub fn for_kind(kind: Kind, width: usize, height: usize, nhwc: bool, side: &Sidecar) -> Prep {
        let (mean, std, fit, crop_pct) = match kind {
            Kind::Classify => (IMAGENET_MEAN, IMAGENET_STD, Fit::Crop, 0.875),
            Kind::Embed => (CLIP_MEAN, CLIP_STD, Fit::Crop, 1.0),
            Kind::Detect => ([0.0; 3], [1.0; 3], Fit::Letterbox, 1.0),
        };
        Prep {
            width,
            height,
            mean: side.mean.unwrap_or(mean),
            std: side.std.unwrap_or(std),
            scale: side.scale.unwrap_or(1.0 / 255.0),
            fit: match side.resize.as_deref() {
                Some("crop") => Fit::Crop,
                Some("stretch") => Fit::Stretch,
                Some("letterbox") => Fit::Letterbox,
                _ => fit,
            },
            crop_pct: side.crop_pct.unwrap_or(crop_pct),
            bgr: side.bgr,
            nhwc,
        }
    }
}

/// The filter of a resampling pass: (support, weight at distance x).
type Kernel = (f32, fn(f32) -> f32);

/// Triangle (bilinear, widened when shrinking so that no pixel is skipped).
const TRIANGLE: Kernel = (1.0, |x| (1.0 - x.abs()).max(0.0));

/// Catmull-Rom cubic (a = -0.5), the bicubic of PIL and so of the Hugging Face image processors.
const CUBIC: Kernel = (2.0, |x| {
    let x = x.abs();
    if x < 1.0 {
        (1.5 * x - 2.5) * x * x + 1.0
    } else if x < 2.0 {
        ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
    } else {
        0.0
    }
});

/// For each of `n` output positions along an axis, the first source pixel and the weights, when the span of `len`
/// source pixels from `start` (fractional) is shrunk or grown to `n`; the filter widens when shrinking.
fn axis_weights(start: f32, len: f32, n: usize, src: usize, k: Kernel) -> Vec<(usize, Vec<f32>)> {
    let scale = len / n as f32;
    let fscale = scale.max(1.0);
    let support = k.0 * fscale;
    (0..n)
        .map(|o| {
            let centre = start + (o as f32 + 0.5) * scale;
            let lo = ((centre - support + 0.5).floor().max(0.0) as usize).min(src - 1);
            let hi = ((centre + support + 0.5).floor().max(0.0) as usize)
                .min(src)
                .max(lo + 1);
            let mut ws: Vec<f32> = (lo..hi).map(|i| (k.1)((i as f32 + 0.5 - centre) / fscale)).collect();
            let sum: f32 = ws.iter().sum();
            if sum.abs() > 1e-6 {
                ws.iter_mut().for_each(|w| *w /= sum);
            } else {
                ws.iter_mut().for_each(|w| *w = 0.0);
                let at = (centre.floor().max(0.0) as usize).clamp(lo, hi - 1);
                ws[at - lo] = 1.0;
            }
            (lo, ws)
        })
        .collect()
}

/// Resamples the part `(x, y, w, h)` (source pixels) of an RGB picture of `size` to `dest`, as 0..255 values rounded
/// after each of the two passes, like PIL. Interleaved RGB.
fn resample(rgb: &[u8], size: (usize, usize), part: (f32, f32, f32, f32), dest: (usize, usize), k: Kernel) -> Vec<f32> {
    let ((w, h), (rx, ry, rw, rh), (dw, dh)) = (size, part, dest);
    let xs = axis_weights(rx, rw, dw, w, k);
    let ys = axis_weights(ry, rh, dh, h, k);
    let y_lo = ys.iter().map(|(lo, _)| *lo).min().unwrap_or(0);
    let y_hi = ys.iter().map(|(lo, ws)| lo + ws.len()).max().unwrap_or(0);
    // the horizontal pass, over the rows that matter
    let mut mid = vec![0f32; (y_hi - y_lo) * dw * 3];
    for y in y_lo..y_hi {
        for (ox, (lo, ws)) in xs.iter().enumerate() {
            for c in 0..3 {
                let v: f32 = ws
                    .iter()
                    .enumerate()
                    .map(|(j, wt)| rgb[(y * w + lo + j) * 3 + c] as f32 * wt)
                    .sum();
                mid[((y - y_lo) * dw + ox) * 3 + c] = v.round().clamp(0.0, 255.0);
            }
        }
    }
    let mut out = vec![0f32; dh * dw * 3];
    for (oy, (lo, ws)) in ys.iter().enumerate() {
        for ox in 0..dw {
            for c in 0..3 {
                let v: f32 = ws
                    .iter()
                    .enumerate()
                    .map(|(j, wt)| mid[((lo + j - y_lo) * dw + ox) * 3 + c] * wt)
                    .sum();
                out[(oy * dw + ox) * 3 + c] = v.round().clamp(0.0, 255.0);
            }
        }
    }
    out
}

/// Resizes an RGB picture (`w` by `h`, 3 bytes a pixel) into the input tensor: `[3, h, w]` or, with `nhwc`,
/// `[h, w, 3]`, as floats `(pixel * scale - mean) / std`. Antialiased: bicubic for a crop (as the Hugging Face
/// processors do), bilinear for stretch and letterbox.
pub fn preprocess(rgb: &[u8], w: usize, h: usize, p: &Prep) -> Vec<f32> {
    let (tw, th) = (p.width, p.height);
    if w == 0 || h == 0 || tw == 0 || th == 0 || rgb.len() < w * h * 3 {
        return vec![0.0; tw * th * 3];
    }
    // the part of the picture that shows, in source pixels, and where it lands in the input
    let (region, (dx, dy, dw, dh), kernel) = match p.fit {
        Fit::Crop => {
            let s = (tw as f32 / w as f32).max(th as f32 / h as f32) / p.crop_pct.clamp(0.01, 1.0);
            let (rw, rh) = ((tw as f32 / s).min(w as f32), (th as f32 / s).min(h as f32));
            (
                ((w as f32 - rw) / 2.0, (h as f32 - rh) / 2.0, rw, rh),
                (0, 0, tw, th),
                CUBIC,
            )
        }
        Fit::Stretch => ((0.0, 0.0, w as f32, h as f32), (0, 0, tw, th), TRIANGLE),
        Fit::Letterbox => ((0.0, 0.0, w as f32, h as f32), letterbox_geom(tw, th, w, h), TRIANGLE),
    };
    let pic = resample(rgb, (w, h), region, (dw, dh), kernel);
    let mut out = vec![0f32; tw * th * 3];
    for oy in 0..th {
        for ox in 0..tw {
            let inside = (dx..dx + dw).contains(&ox) && (dy..dy + dh).contains(&oy);
            for c in 0..3 {
                let sc = if p.bgr { 2 - c } else { c };
                let px = if inside {
                    pic[((oy - dy) * dw + (ox - dx)) * 3 + sc]
                } else {
                    PAD
                };
                let v = (px * p.scale - p.mean[c]) / p.std[c];
                let i = if p.nhwc {
                    (oy * tw + ox) * 3 + c
                } else {
                    c * th * tw + oy * tw + ox
                };
                out[i] = v;
            }
        }
    }
    out
}

// ---- reading the outputs ----------------------------------------------------------------------------------------

/// Softmax, stable against large values.
pub fn softmax(x: &[f32]) -> Vec<f32> {
    let max = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let e: Vec<f32> = x.iter().map(|v| (v - max).exp()).collect();
    let sum: f32 = e.iter().sum();
    e.into_iter().map(|v| v / sum).collect()
}

/// Probabilities from a classifier's output: used as they are when they already are (all within 0..1, summing to
/// about 1), else the softmax of them.
pub fn to_probs(x: &[f32]) -> Vec<f32> {
    let sum: f32 = x.iter().sum();
    if !x.is_empty() && x.iter().all(|v| (0.0..=1.0).contains(v)) && (sum - 1.0).abs() < 1e-2 {
        x.to_vec()
    } else {
        softmax(x)
    }
}

/// The `k` largest values as (index, value), largest first (the lower index first among equals).
pub fn top_k(x: &[f32], k: usize) -> Vec<(usize, f32)> {
    let mut v: Vec<(usize, f32)> = x.iter().copied().enumerate().filter(|(_, s)| !s.is_nan()).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(k);
    v
}

/// Scales a vector to length 1 (a zero vector stays as it is).
pub fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub class: usize,
    pub score: f32,
    /// x1, y1, x2, y2.
    pub bbox: [f32; 4],
}

/// Intersection over union of two boxes.
pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let union =
        (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0) + (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0) - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Non-maximum suppression, per class: the best box stays and the boxes of its class that overlap it by more than
/// `max_iou` go. The result is sorted by score, best first.
pub fn nms(mut dets: Vec<Detection>, max_iou: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in dets {
        if kept
            .iter()
            .all(|k| k.class != d.class || iou(&k.bbox, &d.bbox) <= max_iou)
        {
            kept.push(d);
        }
    }
    kept
}

/// The candidates of a YOLO-style output `[1, rows, cols]`: either `[1, 4 + classes, boxes]` (YOLOv8: centre x,
/// centre y, width, height, class scores) or `[1, boxes, 5 + classes]` (YOLOv5: with an objectness score before the
/// class scores). Boxes are in the model's input pixels, as corners; those under `threshold` are left out.
pub fn decode_yolo(data: &[f32], dims: &[i64], objectness: Option<bool>, threshold: f32) -> Result<Vec<Detection>> {
    let bad = || Fail::new(500, "model_error", format!("not a YOLO-style output: shape {dims:?}"));
    let [batch, d1, d2] = dims else { return Err(bad()) };
    if *batch != 1 || *d1 < 1 || *d2 < 1 || data.len() != (*d1 as usize) * (*d2 as usize) {
        return Err(bad());
    }
    let (d1, d2) = (*d1 as usize, *d2 as usize);
    let channels_first = d1 < d2;
    let (attrs, n) = if channels_first { (d1, d2) } else { (d2, d1) };
    let obj = objectness.unwrap_or(!channels_first);
    let first_class = 4 + obj as usize;
    if attrs <= first_class {
        return Err(bad());
    }
    let get = |i: usize, a: usize| {
        if channels_first {
            data[a * n + i]
        } else {
            data[i * attrs + a]
        }
    };
    let mut out = Vec::new();
    for i in 0..n {
        let (mut class, mut best) = (0, f32::NEG_INFINITY);
        for c in first_class..attrs {
            let s = get(i, c);
            if s > best {
                (class, best) = (c - first_class, s);
            }
        }
        let score = if obj { best * get(i, 4) } else { best };
        if score.is_nan() || score < threshold {
            continue;
        }
        let (cx, cy, w, h) = (get(i, 0), get(i, 1), get(i, 2), get(i, 3));
        out.push(Detection {
            class,
            score,
            bbox: [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0],
        });
    }
    // a model that scores every one of its thousands of boxes high must not make the overlap check slow
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(3000);
    Ok(out)
}

/// The names of the 80 COCO classes, the labels of most YOLO models (used when a detector has 80 classes and its
/// sidecar names none).
pub const COCO_LABELS: [&str; 80] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

// ---- reading the model ------------------------------------------------------------------------------------------

/// What the session says about the model: the one input's shape and the outputs' names and shapes (-1: dynamic).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub input_shape: Vec<i64>,
    pub outputs: Vec<(String, Vec<i64>)>,
}

/// The size of a feature vector when the shape is `[1, n]`, `[n]` or `[1, n, 1, 1]`.
fn flat_features(shape: &[i64]) -> Option<i64> {
    match shape {
        [n] => Some(*n),
        [_, n, rest @ ..] if rest.iter().all(|d| *d == 1) => Some(*n),
        _ => None,
    }
}

/// Which kind of model this is, and which output to read, from the outputs' names and shapes: an output named
/// `*embed*` or `*pooler*` is an embedding; `[1, n]` is a classifier when the sidecar has labels or `n` is 1000 or
/// 1001 (ImageNet), else an embedding; `[1, a, b]` is YOLO-style detection unless named `*hidden*` (a transformer's
/// tokens, an embedding). A sidecar's `kind` overrides all of it.
pub fn guess_kind(outputs: &[(String, Vec<i64>)], labels: usize, forced: Option<Kind>) -> Result<(Kind, usize)> {
    let named = |needles: &[&str]| {
        outputs
            .iter()
            .position(|(n, _)| needles.iter().any(|x| n.to_lowercase().contains(x)))
    };
    if outputs.is_empty() {
        return Err(Fail::new(500, "model_error", "the model has no outputs"));
    }
    if let Some(k) = forced {
        return Ok((
            k,
            if k == Kind::Embed {
                named(&["embed", "pool"]).unwrap_or(0)
            } else {
                0
            },
        ));
    }
    if let Some(i) = named(&["embed", "pooler"]) {
        return Ok((Kind::Embed, i));
    }
    let (name, shape) = &outputs[0];
    if let Some(n) = flat_features(shape) {
        let imagenet = n == 1000 || n == 1001;
        return Ok((
            if labels > 0 || imagenet {
                Kind::Classify
            } else {
                Kind::Embed
            },
            0,
        ));
    }
    if let [_, a, b] = shape.as_slice() {
        if name.to_lowercase().contains("hidden") {
            return Ok((Kind::Embed, 0));
        }
        if [a, b].iter().all(|d| **d <= 0 || **d >= 5) {
            return Ok((Kind::Detect, 0));
        }
    }
    Err(Fail::new(
        500,
        "model_error",
        format!(
            "cannot tell what kind of model this is (first output \"{name}\" has shape {shape:?}); add {{\"kind\": \
             \"classify\" | \"detect\" | \"embed\"}} to its .json file"
        ),
    ))
}

/// What running a model needs, worked out from [`ModelInfo`], the sidecar and the request.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub kind: Kind,
    /// Which output holds the answer.
    pub output: usize,
    pub prep: Prep,
}

pub fn plan(info: &ModelInfo, side: &Sidecar, want: Option<Kind>) -> Result<Plan> {
    let (kind, output) = guess_kind(&info.outputs, side.labels.len(), side.forced_kind())?;
    if let Some(w) = want.filter(|w| *w != kind) {
        return Err(Fail::invalid(format!(
            "this model does {}, not {}: use /v1/vision/{} (or vision.analyze)",
            kind.name(),
            w.name(),
            kind.name()
        )));
    }
    let [_, a, b, c] = info.input_shape.as_slice() else {
        return Err(Fail::new(
            500,
            "model_error",
            format!(
                "the model's input has shape {:?}; an image model takes [1, 3, height, width]",
                info.input_shape
            ),
        ));
    };
    let nhwc = *a != 3 && *c == 3;
    let (h, w) = if nhwc { (*a, *b) } else { (*b, *c) };
    if !nhwc && *a != 3 && *a > 0 {
        return Err(Fail::new(
            500,
            "model_error",
            format!(
                "the model's input has shape {:?}; only 3-channel (colour) models are supported",
                info.input_shape
            ),
        ));
    }
    let (sw, sh) = side.size.map_or((None, None), |s| (Some(s.dims().0), Some(s.dims().1)));
    let fallback = if kind == Kind::Detect { 640 } else { 224 };
    let w = if w > 0 {
        w as usize
    } else {
        sw.unwrap_or(fallback) as usize
    };
    let h = if h > 0 {
        h as usize
    } else {
        sh.unwrap_or(fallback) as usize
    };
    Ok(Plan {
        kind,
        output,
        prep: Prep::for_kind(kind, w, h, nhwc, side),
    })
}

fn round5(v: f32) -> f64 {
    (v as f64 * 1e5).round() / 1e5
}

/// The JSON answer from the output tensor (`shape`, `data`) of the model, for a picture of `img_w` by `img_h`.
pub fn build_result(
    plan: &Plan,
    side: &Sidecar,
    req: &VisionRequest,
    (shape, data): (&[i64], &[f32]),
    (img_w, img_h): (u32, u32),
) -> Result<Value> {
    let label = |i: usize, coco: bool| -> String {
        let named = side.labels.get(i).map(|s| s.as_str());
        let named = named.or(if coco { COCO_LABELS.get(i).copied() } else { None });
        named.map_or_else(|| format!("class {i}"), str::to_string)
    };
    match plan.kind {
        Kind::Classify => {
            if flat_features(shape).is_none_or(|n| n as usize != data.len()) {
                return Err(Fail::new(
                    500,
                    "model_error",
                    format!("not a classifier output: shape {shape:?}"),
                ));
            }
            let probs = to_probs(data);
            let k = req.top_k.unwrap_or(DEFAULT_TOP_K);
            let preds: Vec<Value> = top_k(&probs, k)
                .into_iter()
                .filter(|(_, p)| req.threshold.is_none_or(|t| *p >= t))
                .map(|(i, p)| json!({"class": i, "label": label(i, false), "score": round5(p)}))
                .collect();
            Ok(json!({"kind": "classify", "predictions": preds}))
        }
        Kind::Detect => {
            let mut dets = decode_yolo(data, shape, side.objectness, req.threshold.unwrap_or(DEFAULT_THRESHOLD))?;
            dets = nms(dets, side.nms_iou.unwrap_or(DEFAULT_NMS_IOU));
            dets.truncate(req.top_k.unwrap_or(DEFAULT_MAX_DETECTIONS));
            // input pixels back to the picture: undo the letterbox's padding and scale, or the stretch
            let (ox, oy, kx, ky) = match plan.prep.fit {
                Fit::Letterbox => {
                    let (px, py, nw, nh) =
                        letterbox_geom(plan.prep.width, plan.prep.height, img_w as usize, img_h as usize);
                    (px as f32, py as f32, img_w as f32 / nw as f32, img_h as f32 / nh as f32)
                }
                _ => (
                    0.0,
                    0.0,
                    img_w as f32 / plan.prep.width as f32,
                    img_h as f32 / plan.prep.height as f32,
                ),
            };
            let coco = side.labels.is_empty();
            let items: Vec<Value> = dets
                .iter()
                .map(|d| {
                    let [x1, y1, x2, y2] = d.bbox;
                    let (x1, x2) = (((x1 - ox) * kx).clamp(0.0, img_w as f32), ((x2 - ox) * kx).clamp(0.0, img_w as f32));
                    let (y1, y2) = (((y1 - oy) * ky).clamp(0.0, img_h as f32), ((y2 - oy) * ky).clamp(0.0, img_h as f32));
                    json!({"class": d.class, "label": label(d.class, coco && detector_has_80(shape)), "score": round5(d.score),
                           "box": {"x1": x1.round(), "y1": y1.round(), "x2": x2.round(), "y2": y2.round()}})
                })
                .collect();
            Ok(json!({"kind": "detect", "image": {"width": img_w, "height": img_h}, "detections": items}))
        }
        Kind::Embed => {
            let mut v: Vec<f32> = match (flat_features(shape), shape) {
                (Some(n), _) if n as usize == data.len() => data.to_vec(),
                // a transformer's tokens [1, tokens, dim]: the first one (CLS) stands for the picture
                (None, [1, _, dim]) if *dim > 0 && data.len() >= *dim as usize => data[..*dim as usize].to_vec(),
                _ => {
                    return Err(Fail::new(
                        500,
                        "model_error",
                        format!("not an embedding output: shape {shape:?}"),
                    ))
                }
            };
            if side.normalize.unwrap_or(true) {
                l2_normalize(&mut v);
            }
            Ok(json!({"kind": "embed", "dimensions": v.len(), "embedding": v}))
        }
    }
}

/// Is `shape` a YOLO output with 80 classes (so COCO's names apply)? v8 has 84 attributes along the short side, v5 85.
fn detector_has_80(shape: &[i64]) -> bool {
    match shape {
        [_, a, b] if a < b => *a == 84,
        [_, _, b] => *b == 85,
        _ => false,
    }
}

/// How many values of an embedding the CLI's text view shows.
const EMBED_PREVIEW: usize = 6;

/// The CLI's plain-text view of an answer: one line per prediction or box; an embedding as its size and first values.
pub fn render_text(v: &Value) -> String {
    let mut out = String::new();
    let score = |x: &Value| x["score"].as_f64().unwrap_or(0.0);
    match v["kind"].as_str() {
        Some("classify") => {
            for p in v["predictions"].as_array().into_iter().flatten() {
                out += &format!(
                    "{:6.2}%  {}
",
                    score(p) * 100.0,
                    p["label"].as_str().unwrap_or("")
                );
            }
        }
        Some("detect") => {
            for d in v["detections"].as_array().into_iter().flatten() {
                let b = &d["box"];
                out += &format!(
                    "{:6.2}%  {}  [{}, {}, {}, {}]
",
                    score(d) * 100.0,
                    d["label"].as_str().unwrap_or(""),
                    b["x1"],
                    b["y1"],
                    b["x2"],
                    b["y2"]
                );
            }
            if out.is_empty() {
                out.push_str(
                    "nothing found
",
                );
            }
        }
        Some("embed") => {
            let all = v["embedding"].as_array().map(Vec::as_slice).unwrap_or_default();
            let head: Vec<String> = all
                .iter()
                .take(EMBED_PREVIEW)
                .map(|x| format!("{:.4}", x.as_f64().unwrap_or(0.0)))
                .collect();
            out += &format!(
                "{} dimensions: [{}{}] (--json for the whole vector)\n",
                all.len(),
                head.join(", "),
                if all.len() > EMBED_PREVIEW { ", ..." } else { "" }
            );
        }
        _ => {
            out += &format!(
                "{v}
"
            )
        }
    }
    out
}

// ---- running ----------------------------------------------------------------------------------------------------

/// Decodes a PNG or JPEG into (width, height, RGB bytes).
#[cfg(feature = "vision")]
pub fn decode_image(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let bad = |e: image::ImageError| Fail::invalid(format!("cannot read the image (PNG and JPEG are supported): {e}"));
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Fail::invalid(format!("cannot read the image: {e}")))?;
    let (w, h) = reader.into_dimensions().map_err(bad)?;
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Fail::invalid(format!(
            "the image is {w}x{h}, over {} megapixels",
            MAX_PIXELS / 1_000_000
        )));
    }
    let img = image::load_from_memory(bytes).map_err(bad)?.to_rgb8();
    Ok((img.width(), img.height(), img.into_raw()))
}

#[cfg(feature = "vision")]
mod session {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::SystemTime;

    use ort::session::Session;
    use ort::value::{Tensor, TensorElementType, ValueType};

    use super::*;

    pub struct Loaded {
        pub session: Session,
        pub info: ModelInfo,
        pub side: Sidecar,
    }

    fn fail(path: &Path, e: impl std::fmt::Display) -> Fail {
        Fail::new(500, "model_error", format!("{}: {e}", path.display()))
    }

    fn shape_of(dt: &ValueType) -> Option<Vec<i64>> {
        match dt {
            ValueType::Tensor { ty, shape, .. } if *ty == TensorElementType::Float32 => Some(shape.to_vec()),
            _ => None,
        }
    }

    fn open(path: &Path) -> Result<Loaded> {
        let side = Sidecar::load(path)?;
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(path))
            .map_err(|e| fail(path, e))?;
        let inputs = session.inputs();
        if inputs.len() != 1 {
            return Err(fail(
                path,
                format!(
                    "the model takes {} inputs; an image model takes just the picture",
                    inputs.len()
                ),
            ));
        }
        let input_shape = shape_of(inputs[0].dtype()).ok_or_else(|| {
            fail(
                path,
                "the model's input is not a float32 tensor (only float32 models are supported)",
            )
        })?;
        let mut outputs = Vec::new();
        for o in session.outputs() {
            if let Some(s) = shape_of(o.dtype()) {
                outputs.push((o.name().to_string(), s));
            }
        }
        Ok(Loaded {
            session,
            info: ModelInfo { input_shape, outputs },
            side,
        })
    }

    /// One loaded model is kept: the next request for it skips reading the file again.
    type Slot = Mutex<Option<(PathBuf, SystemTime, Arc<Mutex<Loaded>>)>>;
    static LOADED: OnceLock<Slot> = OnceLock::new();

    fn load(path: &Path) -> Result<Arc<Mutex<Loaded>>> {
        let stamp = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut slot = LOADED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, m)) = slot.as_ref().filter(|(p, t, _)| p == path && *t == stamp) {
            return Ok(m.clone());
        }
        let m = Arc::new(Mutex::new(open(path)?));
        *slot = Some((path.to_path_buf(), stamp, m.clone()));
        Ok(m)
    }

    /// Runs `model` on the picture. `want` is the kind the route asked for (none: whatever the model is).
    pub fn analyze(model: &Path, req: &VisionRequest, want: Option<Kind>) -> Result<Value> {
        let (w, h, rgb) = decode_image(&req.image)?;
        let loaded = load(model)?;
        let mut guard = loaded.lock().unwrap_or_else(|e| e.into_inner());
        let l = &mut *guard;
        let plan = plan(&l.info, &l.side, want)?;
        let pixels = preprocess(&rgb, w as usize, h as usize, &plan.prep);
        let dims = if plan.prep.nhwc {
            [1, plan.prep.height as i64, plan.prep.width as i64, 3]
        } else {
            [1, 3, plan.prep.height as i64, plan.prep.width as i64]
        };
        let tensor = Tensor::from_array((dims, pixels)).map_err(|e| fail(model, e))?;
        let outputs = l.session.run(ort::inputs![tensor]).map_err(|e| fail(model, e))?;
        let name = &l.info.outputs[plan.output].0;
        let (shape, data) = outputs[name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| fail(model, e))?;
        let shape: Vec<i64> = shape.to_vec();
        let mut v = build_result(&plan, &l.side, req, (&shape, data), (w, h))?;
        v["model"] = json!(model_name(model).unwrap_or_default());
        Ok(v)
    }
}

/// Runs the model at `model` on the request's picture and answers in JSON (see docs/vision.md). `want` is the kind
/// a route asked for: a model of another kind is refused with a message that names the right route.
#[cfg(feature = "vision")]
pub fn analyze(model: &Path, req: &VisionRequest, want: Option<Kind>) -> Result<Value> {
    session::analyze(model, req, want)
}

#[cfg(not(feature = "vision"))]
pub fn analyze(_: &Path, _: &VisionRequest, _: Option<Kind>) -> Result<Value> {
    Err(Fail::new(501, "not_implemented", NOT_BUILT))
}

/// A tiny ONNX model written by hand (protobuf), so that the session path is tested without a model file:
/// picture [1,3,4,4] -> GlobalAveragePool -> Flatten -> Softmax, whose 3 classes are the share of red, green, blue.
#[cfg(all(test, feature = "vision"))]
pub(crate) mod test_model {

    fn varint(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn field(num: u64, wire: u64, payload: &[u8], out: &mut Vec<u8>) {
        varint(num << 3 | wire, out);
        if wire == 2 {
            varint(payload.len() as u64, out);
        }
        out.extend_from_slice(payload);
    }

    fn bytes(num: u64, p: &[u8], out: &mut Vec<u8>) {
        field(num, 2, p, out);
    }

    fn int(num: u64, n: u64, out: &mut Vec<u8>) {
        let mut v = Vec::new();
        varint(n, &mut v);
        field(num, 0, &v, out);
    }

    fn value_info(name: &str, dims: &[u64]) -> Vec<u8> {
        let mut shape = Vec::new();
        for d in dims {
            let mut dim = Vec::new();
            int(1, *d, &mut dim);
            bytes(1, &dim, &mut shape);
        }
        let mut tensor = Vec::new();
        int(1, 1, &mut tensor); // float
        bytes(2, &shape, &mut tensor);
        let mut ty = Vec::new();
        bytes(1, &tensor, &mut ty);
        let mut vi = Vec::new();
        bytes(1, name.as_bytes(), &mut vi);
        bytes(2, &ty, &mut vi);
        vi
    }

    fn node(op: &str, input: &str, output: &str, attr: Option<(&str, u64)>) -> Vec<u8> {
        let mut n = Vec::new();
        bytes(1, input.as_bytes(), &mut n);
        bytes(2, output.as_bytes(), &mut n);
        bytes(3, format!("{op}_node").as_bytes(), &mut n);
        bytes(4, op.as_bytes(), &mut n);
        if let Some((name, v)) = attr {
            let mut a = Vec::new();
            bytes(1, name.as_bytes(), &mut a);
            int(3, v, &mut a);
            int(20, 2, &mut a); // INT
            bytes(5, &a, &mut n);
        }
        n
    }

    pub fn color_share_model() -> Vec<u8> {
        let mut g = Vec::new();
        bytes(1, &node("GlobalAveragePool", "picture", "pooled", None), &mut g);
        bytes(1, &node("Flatten", "pooled", "flat", Some(("axis", 1))), &mut g);
        bytes(1, &node("Softmax", "flat", "scores", Some(("axis", 1))), &mut g);
        bytes(2, b"color_share", &mut g);
        bytes(11, &value_info("picture", &[1, 3, 4, 4]), &mut g);
        bytes(12, &value_info("scores", &[1, 3]), &mut g);
        let mut opset = Vec::new();
        int(2, 13, &mut opset);
        let mut m = Vec::new();
        int(1, 8, &mut m); // ir_version
        bytes(8, &opset, &mut m);
        bytes(7, &g, &mut m);
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn prep(w: usize, h: usize) -> Prep {
        Prep {
            width: w,
            height: h,
            mean: [0.0; 3],
            std: [1.0; 3],
            scale: 1.0 / 255.0,
            fit: Fit::Stretch,
            crop_pct: 1.0,
            bgr: false,
            nhwc: false,
        }
    }

    fn solid(w: usize, h: usize, px: [u8; 3]) -> Vec<u8> {
        (0..w * h).flat_map(|_| px).collect()
    }

    #[test]
    fn names_and_lookup() {
        assert_eq!(model_name(Path::new("a/resnet50.onnx")).as_deref(), Some("resnet50"));
        assert_eq!(model_name(Path::new("a/CLIP.ONNX")).as_deref(), Some("CLIP"));
        assert_eq!(model_name(Path::new("a/resnet50.json")), None);
        assert_eq!(model_name(Path::new("a/.onnx")), None);
        let d = tmp();
        let first = d.join("vision");
        std::fs::create_dir_all(first.join("sub")).unwrap();
        std::fs::write(first.join("a.onnx"), b"x").unwrap();
        std::fs::write(first.join("a.json"), b"{}").unwrap();
        std::fs::write(first.join("sub").join("B.onnx"), b"x").unwrap();
        std::fs::write(d.join("a.onnx"), b"other").unwrap();
        let dirs = vec![first.clone(), d.clone(), d.join("missing")];
        let names: Vec<String> = list_models(&dirs).into_iter().map(|m| m.0).collect();
        assert_eq!(names, ["a", "B"]);
        assert_eq!(find_model(&dirs, "b").unwrap().0, "B");
        assert_eq!(find_model(&dirs, "a.onnx").unwrap().1, first.join("a.onnx"));
        let e = find_model(&dirs, "zzz").unwrap_err();
        assert!(e.status == 404 && e.message.contains("zzz"), "{e:?}");
        // no name: ambiguous with two, fine with one
        assert_eq!(find_model(&dirs, "").unwrap_err().status, 400);
        assert_eq!(find_model(&[first.join("sub")], "").unwrap().0, "B");
        assert_eq!(find_model(&[d.join("missing")], "").unwrap_err().status, 404);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sidecar_parsing() {
        let s = Sidecar::parse(
            r#"{"kind": "Detect", "labels": ["a", "b"], "mean": [0.5, 0.5, 0.5], "std": [0.5, 0.5, 0.5],
                "size": [320, 256], "resize": "crop", "bgr": true, "objectness": false, "nms_iou": 0.5}"#,
        )
        .unwrap();
        assert_eq!(s.forced_kind(), Some(Kind::Detect));
        assert_eq!(s.labels, ["a", "b"]);
        assert_eq!(s.size.unwrap().dims(), (320, 256));
        assert!(s.bgr && s.objectness == Some(false));
        assert_eq!(
            Sidecar::parse(r#"{"size": 224}"#).unwrap().size.unwrap().dims(),
            (224, 224)
        );
        assert_eq!(Sidecar::parse("{}").unwrap(), Sidecar::default());
        for bad in [
            r#"{"kind": "segment"}"#,
            r#"{"std": [1, 0, 1]}"#,
            r#"{"size": 4}"#,
            r#"{"size": [64]}"#,
            r#"{"resize": "pad"}"#,
            r#"{"nms_iou": 2}"#,
            r#"{"labelz": []}"#,
            "not json",
        ] {
            assert!(Sidecar::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sidecar_is_found_beside_the_model() {
        let d = tmp();
        let m = d.join("net.onnx");
        assert_eq!(Sidecar::load(&m).unwrap(), Sidecar::default());
        std::fs::write(d.join("net.onnx.json"), r#"{"labels": ["x"]}"#).unwrap();
        assert_eq!(Sidecar::load(&m).unwrap().labels, ["x"]);
        std::fs::write(d.join("net.json"), r#"{"labels": ["y"]}"#).unwrap();
        assert_eq!(Sidecar::load(&m).unwrap().labels, ["y"]);
        std::fs::write(d.join("net.json"), "{").unwrap();
        assert_eq!(Sidecar::load(&m).unwrap_err().status, 500);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn base64_decoding() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("-_8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("a$b=").is_err());
        assert!(decode_base64("aGVsbG8=x").is_err());
        assert!(decode_base64("a").is_err());
        for n in 0..20u8 {
            let bytes: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(decode_base64(&crate::image::base64(&bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn request_parsing() {
        use serde_json::json;
        let img = crate::image::base64(b"\x89PNG data");
        let r = VisionRequest::from_json(
            &json!({"model": " net ", "image": img, "top_k": 3, "threshold": 0.5}),
            false,
        )
        .unwrap();
        assert_eq!((r.model.as_str(), r.top_k, r.threshold), ("net", Some(3), Some(0.5)));
        assert_eq!(r.image, b"\x89PNG data");
        let url = format!("data:image/png;base64,{img}");
        assert_eq!(
            VisionRequest::from_json(&json!({"image": url}), false).unwrap().image,
            r.image
        );
        assert_eq!(
            VisionRequest::from_json(&json!({"image": img}), false).unwrap().model,
            ""
        );
        for (body, needle) in [
            (json!({}), "image is required"),
            (json!([]), "JSON object"),
            (json!({"image": 5}), "image must be a string"),
            (json!({"image": "!!"}), "base64"),
            (json!({"image": "data:image/png,abc"}), "base64"),
            (json!({"image": "data:image/png;base64"}), "comma"),
            (json!({"image": img, "top_k": 0}), "top_k"),
            (json!({"image": img, "top_k": 1.5}), "top_k"),
            (json!({"image": img, "threshold": 2}), "threshold"),
            (json!({"image": img, "threshold": "x"}), "threshold"),
            (json!({"image": img, "model": 3}), "model must be a string"),
        ] {
            let e = VisionRequest::from_json(&body, false).unwrap_err();
            assert!(e.contains(needle), "{body}: {e}");
        }
        // `file` only where it is allowed
        let d = tmp();
        let f = d.join("p.png");
        std::fs::write(&f, b"bytes").unwrap();
        let body = json!({"file": f.display().to_string()});
        assert!(VisionRequest::from_json(&body, false)
            .unwrap_err()
            .contains("image is required"));
        assert_eq!(VisionRequest::from_json(&body, true).unwrap().image, b"bytes");
        assert!(VisionRequest::from_json(&json!({"file": d.join("none").display().to_string()}), true).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preprocess_makes_nchw_and_nhwc() {
        // a 4x4 red picture to 2x2: red plane 1, the others 0
        let v = preprocess(&solid(4, 4, [255, 0, 0]), 4, 4, &prep(2, 2));
        assert_eq!(v.len(), 12);
        assert!(v[..4].iter().all(|x| (x - 1.0).abs() < 1e-6), "{v:?}");
        assert!(v[4..].iter().all(|x| x.abs() < 1e-6));
        let mut p = prep(2, 2);
        p.nhwc = true;
        let v = preprocess(&solid(4, 4, [255, 0, 0]), 4, 4, &p);
        assert_eq!(&v[..6], &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        // blue, green, red order
        let mut p = prep(1, 1);
        p.bgr = true;
        assert_eq!(
            preprocess(&solid(2, 2, [255, 128, 0]), 2, 2, &p),
            [0.0, 128.0 / 255.0, 1.0]
        );
        // mean, std and scale
        let mut p = prep(1, 1);
        p.mean = [0.5, 0.0, 0.0];
        p.std = [0.25, 1.0, 1.0];
        p.scale = 1.0 / 255.0;
        let v = preprocess(&solid(3, 3, [255, 255, 0]), 3, 3, &p);
        assert!(
            (v[0] - 2.0).abs() < 1e-5 && (v[1] - 1.0).abs() < 1e-5 && v[2].abs() < 1e-5,
            "{v:?}"
        );
        p.scale = 1.0;
        p.mean = [0.0; 3];
        p.std = [1.0; 3];
        assert_eq!(preprocess(&solid(1, 1, [7, 8, 9]), 1, 1, &p), [7.0, 8.0, 9.0]);
        // not enough pixels: zeros, not a panic
        assert_eq!(preprocess(&[1, 2, 3], 4, 4, &prep(2, 2)), vec![0.0; 12]);
    }

    /// 4 wide, 2 high: the left half red, the right half blue.
    fn halves() -> Vec<u8> {
        (0..8)
            .flat_map(|i| if i % 4 < 2 { [255, 0, 0] } else { [0, 0, 255] })
            .collect()
    }

    #[test]
    fn stretch_keeps_the_whole_picture_and_crop_the_centre() {
        let p = prep(2, 2);
        let v = preprocess(&halves(), 4, 2, &p);
        // red plane: the left column mostly red, the right mostly blue (the filter is wide when shrinking)
        assert!(v[0] > 0.7 && v[1] < 0.3 && v[2] > 0.7 && v[3] < 0.3, "{v:?}");
        let mut c = prep(2, 2);
        c.fit = Fit::Crop;
        // the centre 2x2 is one red and one blue column too, but at the pixel level: red at x=1, blue at x=2
        let v = preprocess(&halves(), 4, 2, &c);
        assert_eq!(&v[..4], &[1.0, 0.0, 1.0, 0.0]);
        // a 1-wide target takes the centre pixel pair, which is half red, half blue
        let mut c = prep(1, 2);
        c.fit = Fit::Crop;
        let v = preprocess(&halves(), 4, 2, &c);
        // red plane first, blue plane last
        assert!((v[0] - 0.5).abs() < 0.01 && (v[4] - 0.5).abs() < 0.01, "{v:?}");
    }

    #[test]
    fn shrinking_a_lot_averages() {
        // a checkerboard of black and white 8x8, to 1x1: grey
        let px: Vec<u8> = (0..64)
            .flat_map(|i| if (i / 8 + i % 8) % 2 == 0 { [255; 3] } else { [0; 3] })
            .collect();
        let v = preprocess(&px, 8, 8, &prep(1, 1));
        assert!((v[0] - 0.5).abs() < 0.01, "{v:?}");
        // an odd size reduces too (edge blocks are smaller)
        let v = preprocess(&solid(9, 7, [255, 255, 255]), 9, 7, &prep(1, 1));
        assert!((v[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn softmax_and_top_k() {
        let p = softmax(&[1.0, 2.0, 3.0]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(p[2] > p[1] && p[1] > p[0]);
        // large logits do not overflow
        let big = softmax(&[1000.0, 1001.0]);
        assert!(big.iter().all(|x| x.is_finite()) && (big[1] - 0.731_058_6).abs() < 1e-5);
        // probabilities pass through, logits do not
        assert_eq!(to_probs(&[0.25, 0.75]), [0.25, 0.75]);
        assert_ne!(to_probs(&[0.25, 0.25]), [0.25, 0.25]);
        assert!((to_probs(&[3.0, -1.0]).iter().sum::<f32>() - 1.0).abs() < 1e-6);
        let t = top_k(&[0.1, 0.5, 0.5, 0.2, f32::NAN], 3);
        assert_eq!(t, [(1, 0.5), (2, 0.5), (3, 0.2)]);
        assert_eq!(top_k(&[1.0], 5).len(), 1);
        assert!(top_k(&[], 3).is_empty());
        let mut v = [3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        let mut z = [0.0, 0.0];
        l2_normalize(&mut z);
        assert_eq!(z, [0.0, 0.0]);
    }

    fn det(class: usize, score: f32, bbox: [f32; 4]) -> Detection {
        Detection { class, score, bbox }
    }

    #[test]
    fn overlap_and_nms() {
        let a = [0.0, 0.0, 10.0, 10.0];
        assert_eq!(iou(&a, &a), 1.0);
        assert_eq!(iou(&a, &[20.0, 20.0, 30.0, 30.0]), 0.0);
        assert!((iou(&a, &[5.0, 0.0, 15.0, 10.0]) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(iou(&[1.0, 1.0, 1.0, 1.0], &[1.0, 1.0, 1.0, 1.0]), 0.0);
        let kept = nms(
            vec![
                det(0, 0.6, [1.0, 1.0, 11.0, 11.0]),
                det(0, 0.9, [0.0, 0.0, 10.0, 10.0]),
                // overlaps the best one, but another class: stays
                det(1, 0.7, [0.0, 0.0, 10.0, 10.0]),
                // far away, same class: stays
                det(0, 0.5, [50.0, 50.0, 60.0, 60.0]),
            ],
            0.45,
        );
        let scores: Vec<f32> = kept.iter().map(|d| d.score).collect();
        assert_eq!(scores, [0.9, 0.7, 0.5]);
        assert!(nms(vec![], 0.5).is_empty());
    }

    type Box8<'a> = (usize, f32, f32, f32, f32, &'a [f32]);

    /// A YOLOv8-style output `[1, 4 + classes, n]` of `n` boxes; `boxes` are (column, cx, cy, w, h, class scores).
    fn v8_output(n: usize, classes: usize, boxes: &[Box8]) -> (Vec<i64>, Vec<f32>) {
        let attrs = 4 + classes;
        let mut data = vec![0.0f32; attrs * n];
        for (col, cx, cy, w, h, scores) in boxes {
            for (a, v) in [*cx, *cy, *w, *h].iter().chain(scores.iter()).enumerate() {
                data[a * n + col] = *v;
            }
        }
        (vec![1, attrs as i64, n as i64], data)
    }

    #[test]
    fn yolo_v8_layout_decodes() {
        let (dims, data) = v8_output(
            8,
            2,
            &[
                (0, 10.0, 10.0, 4.0, 6.0, &[0.9, 0.1]),
                (3, 50.0, 50.0, 8.0, 8.0, &[0.1, 0.8]),
                (5, 90.0, 90.0, 2.0, 2.0, &[0.0, 0.2]),
            ],
        );
        let d = decode_yolo(&data, &dims, None, 0.25).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].class, d[0].score, d[0].bbox), (0, 0.9, [8.0, 7.0, 12.0, 13.0]));
        assert_eq!((d[1].class, d[1].bbox), (1, [46.0, 46.0, 54.0, 54.0]));
        assert_eq!(decode_yolo(&data, &dims, None, 0.95).unwrap().len(), 0);
    }

    #[test]
    fn yolo_v5_layout_decodes() {
        // [1, boxes, 5 + 2 classes]: rows are boxes; the score is objectness * class score
        let mut data = vec![0.0f32; 8 * 7];
        data[..7].copy_from_slice(&[20.0, 20.0, 10.0, 10.0, 0.5, 0.2, 0.8]);
        data[7..14].copy_from_slice(&[60.0, 60.0, 10.0, 10.0, 0.9, 0.9, 0.1]);
        let d = decode_yolo(&data, &[1, 8, 7], None, 0.3).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].class, d[0].bbox), (0, [55.0, 55.0, 65.0, 65.0]));
        assert!((d[0].score - 0.81).abs() < 1e-6 && (d[1].score - 0.4).abs() < 1e-6);
        // forcing "no objectness" reads the 5th value as a class score
        let d = decode_yolo(&data, &[1, 8, 7], Some(false), 0.85).unwrap();
        assert_eq!(d.len(), 1);
        for bad in [&[1i64, 7][..], &[2, 8, 7], &[1, 8, 6], &[1, 3, 7]] {
            assert!(decode_yolo(&data, bad, None, 0.1).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn yolo_picks_the_class_with_the_top_score_in_both_layouts() {
        // 80 classes, one box whose best class is 16 (dog) while person (0) scores second
        let mut scores = vec![0.0f32; 80];
        scores[0] = 0.4;
        scores[16] = 0.7;
        scores[79] = 0.1;
        let (dims, data) = v8_output(100, 80, &[(7, 100.0, 120.0, 40.0, 60.0, &scores)]);
        let d = decode_yolo(&data, &dims, None, 0.25).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(
            (d[0].class, d[0].score, d[0].bbox),
            (16, 0.7, [80.0, 90.0, 120.0, 150.0])
        );
        // the same boxes as [1, boxes, 84], told there is no objectness
        let (n, attrs) = (100, 84);
        let mut t = vec![0.0f32; n * attrs];
        for a in 0..attrs {
            t[7 * attrs + a] = data[a * n + 7];
        }
        let d = decode_yolo(&t, &[1, n as i64, attrs as i64], Some(false), 0.25).unwrap();
        assert_eq!((d.len(), d[0].class, d[0].score), (1, 16, 0.7));
        // and the label is dog
        let (shape, flat) = (dims, data);
        let plan = Plan {
            kind: Kind::Detect,
            prep: Prep::for_kind(Kind::Detect, 640, 640, false, &Sidecar::default()),
            output: 0,
        };
        let r = build_result(
            &plan,
            &Sidecar::default(),
            &req(None, None),
            (&shape, &flat),
            (640, 640),
        )
        .unwrap();
        assert_eq!(r["detections"][0]["label"], "dog");
    }

    #[test]
    fn letterbox_pads_with_grey_and_boxes_map_back() {
        // a 4x2 white picture in a 4x4 input: 4x2 in the middle (rows 1 and 2), grey above and below
        let mut p = prep(4, 4);
        p.fit = Fit::Letterbox;
        let v = preprocess(&solid(4, 2, [255, 255, 255]), 4, 2, &p);
        let red = &v[..16];
        let grey = 114.0 / 255.0;
        assert!((red[0] - grey).abs() < 1e-5 && (red[12] - grey).abs() < 1e-5, "{red:?}");
        assert!((red[4] - 1.0).abs() < 1e-5 && (red[11] - 1.0).abs() < 1e-5, "{red:?}");
        // a box on the picture's right half comes back in picture pixels, the padding removed
        let plan = Plan {
            kind: Kind::Detect,
            prep: Prep {
                fit: Fit::Letterbox,
                ..prep(640, 640)
            },
            output: 0,
        };
        // a 1280x640 picture: scale 0.5, 160 rows of padding above
        let (shape, data) = v8_output(16, 1, &[(1, 480.0, 320.0, 100.0, 100.0, &[0.9])]);
        let r = build_result(
            &plan,
            &Sidecar::default(),
            &req(None, None),
            (&shape, &data),
            (1280, 640),
        )
        .unwrap();
        let b = &r["detections"][0]["box"];
        assert_eq!(
            (b["x1"].as_f64(), b["y1"].as_f64(), b["x2"].as_f64(), b["y2"].as_f64()),
            (Some(860.0), Some(220.0), Some(1060.0), Some(420.0))
        );
    }

    #[test]
    fn classifier_crop_keeps_the_centre_share() {
        // an 8x4 picture whose two outer columns each side are blue and the middle four red; a 2x2 input with
        // crop_pct 0.5 shows only the middle 2x2 pixels, all red
        let px: Vec<u8> = (0..4)
            .flat_map(|_| (0..8).flat_map(|x| if (2..6).contains(&x) { [255, 0, 0] } else { [0, 0, 255] }))
            .collect();
        let mut p = prep(2, 2);
        p.fit = Fit::Crop;
        p.crop_pct = 0.5;
        let v = preprocess(&px, 8, 4, &p);
        assert!(v[..4].iter().all(|x| *x > 0.99), "{v:?}");
        assert!(Sidecar::parse(r#"{"crop_pct": 0}"#).is_err());
        assert!(Sidecar::parse(r#"{"resize": "letterbox", "crop_pct": 0.875}"#).is_ok());
    }

    fn outs(shapes: &[(&str, &[i64])]) -> Vec<(String, Vec<i64>)> {
        shapes.iter().map(|(n, s)| (n.to_string(), s.to_vec())).collect()
    }

    #[test]
    fn the_kind_is_guessed_from_the_outputs() {
        let g = |o: &[(&str, &[i64])], labels, forced| guess_kind(&outs(o), labels, forced);
        assert_eq!(g(&[("logits", &[1, 1000])], 0, None).unwrap(), (Kind::Classify, 0));
        assert_eq!(g(&[("logits", &[-1, 1001])], 0, None).unwrap(), (Kind::Classify, 0));
        assert_eq!(g(&[("out", &[1, 10])], 10, None).unwrap(), (Kind::Classify, 0));
        assert_eq!(g(&[("out", &[1, 1000, 1, 1])], 0, None).unwrap(), (Kind::Classify, 0));
        assert_eq!(g(&[("out", &[1, 512])], 0, None).unwrap(), (Kind::Embed, 0));
        assert_eq!(
            g(&[("last", &[1, 4]), ("image_embeds", &[1, 512])], 0, None).unwrap(),
            (Kind::Embed, 1)
        );
        assert_eq!(g(&[("pooler_output", &[1, 768])], 5, None).unwrap(), (Kind::Embed, 0));
        assert_eq!(g(&[("output0", &[1, 84, 8400])], 0, None).unwrap(), (Kind::Detect, 0));
        assert_eq!(g(&[("output", &[1, 25200, 85])], 0, None).unwrap(), (Kind::Detect, 0));
        assert_eq!(g(&[("output0", &[-1, -1, -1])], 0, None).unwrap(), (Kind::Detect, 0));
        assert_eq!(
            g(&[("last_hidden_state", &[1, 197, 768])], 0, None).unwrap(),
            (Kind::Embed, 0)
        );
        // the sidecar's kind wins
        assert_eq!(g(&[("out", &[1, 1000])], 0, Some(Kind::Embed)).unwrap().0, Kind::Embed);
        let e = g(&[("x", &[1, 2, 3, 4, 5])], 0, None).unwrap_err();
        assert!(e.message.contains("kind"), "{e:?}");
        assert!(g(&[], 0, None).is_err());
    }

    fn info(input: &[i64], outputs: &[(&str, &[i64])]) -> ModelInfo {
        ModelInfo {
            input_shape: input.to_vec(),
            outputs: outs(outputs),
        }
    }

    #[test]
    fn the_plan_follows_the_model_and_the_sidecar() {
        let none = Sidecar::default();
        let p = plan(&info(&[1, 3, 224, 224], &[("o", &[1, 1000])]), &none, None).unwrap();
        assert_eq!(
            (p.kind, p.prep.width, p.prep.height, p.prep.nhwc, p.prep.fit),
            (Kind::Classify, 224, 224, false, Fit::Crop)
        );
        assert_eq!(p.prep.mean, IMAGENET_MEAN);
        // channels last
        let p = plan(&info(&[1, 160, 192, 3], &[("o", &[1, 1000])]), &none, None).unwrap();
        assert_eq!((p.prep.width, p.prep.height, p.prep.nhwc), (192, 160, true));
        // dynamic sides: the sidecar's size, else a default for the kind
        let dynamic = info(&[1, 3, -1, -1], &[("o", &[1, 84, 8400])]);
        let p = plan(&dynamic, &none, None).unwrap();
        assert_eq!(
            (p.kind, p.prep.width, p.prep.height, p.prep.fit),
            (Kind::Detect, 640, 640, Fit::Letterbox)
        );
        let side = Sidecar::parse(r#"{"size": [320, 256], "mean": [1, 2, 3], "scale": 1}"#).unwrap();
        let p = plan(&dynamic, &side, None).unwrap();
        assert_eq!(
            (p.prep.width, p.prep.height, p.prep.mean, p.prep.scale),
            (320, 256, [1.0, 2.0, 3.0], 1.0)
        );
        // a static input size is not changed by the sidecar
        let p = plan(&info(&[1, 3, 100, 100], &[("o", &[1, 1000])]), &side, None).unwrap();
        assert_eq!(p.prep.width, 100);
        // a route for another kind is refused, naming the right one
        let e = plan(&info(&[1, 3, 224, 224], &[("o", &[1, 1000])]), &none, Some(Kind::Embed)).unwrap_err();
        assert!(e.status == 400 && e.message.contains("/v1/vision/classify"), "{e:?}");
        assert!(plan(
            &info(&[1, 3, 224, 224], &[("o", &[1, 1000])]),
            &none,
            Some(Kind::Classify)
        )
        .is_ok());
        // inputs that are not a colour picture
        assert!(plan(&info(&[1, 1, 28, 28], &[("o", &[1, 10])]), &none, None).is_err());
        assert!(plan(&info(&[1, 224], &[("o", &[1, 10])]), &none, None).is_err());
    }

    fn req(top_k: Option<usize>, threshold: Option<f32>) -> VisionRequest {
        VisionRequest {
            model: String::new(),
            image: vec![],
            top_k,
            threshold,
        }
    }

    #[test]
    fn answers_are_built_from_the_output_tensor() {
        let none = Sidecar::default();
        let side = Sidecar::parse(r#"{"labels": ["cat", "dog", "fox"]}"#).unwrap();
        let cp = plan(&info(&[1, 3, 8, 8], &[("o", &[1, 3])]), &side, None).unwrap();
        let v = build_result(&cp, &side, &req(Some(2), None), (&[1, 3], &[0.1, 3.0, 1.0]), (10, 10)).unwrap();
        assert_eq!(v["kind"], "classify");
        let p = v["predictions"].as_array().unwrap();
        assert_eq!(
            (p.len(), p[0]["label"].as_str(), p[0]["class"].as_u64()),
            (2, Some("dog"), Some(1))
        );
        assert!(p[0]["score"].as_f64().unwrap() > p[1]["score"].as_f64().unwrap());
        // a threshold drops weak ones; a missing label is "class N"
        let v = build_result(&cp, &none, &req(None, Some(0.5)), (&[1, 3], &[0.1, 3.0, 1.0]), (10, 10)).unwrap();
        assert_eq!(v["predictions"].as_array().unwrap().len(), 1);
        assert_eq!(v["predictions"][0]["label"], "class 1");
        assert!(build_result(&cp, &side, &req(None, None), (&[1, 4], &[0.1, 3.0, 1.0]), (1, 1)).is_err());

        // detection: boxes come back in the picture's pixels (the model sees 100x100, the picture is 200x50)
        let dp = Plan {
            kind: Kind::Detect,
            output: 0,
            prep: prep(100, 100),
        };
        let (dims, data) = v8_output(
            8,
            2,
            &[
                (1, 50.0, 50.0, 20.0, 40.0, &[0.9, 0.0]),
                (4, 52.0, 50.0, 20.0, 40.0, &[0.8, 0.0]),
            ],
        );
        let v = build_result(&dp, &none, &req(None, None), (&dims, &data), (200, 50)).unwrap();
        let d = v["detections"].as_array().unwrap();
        assert_eq!(d.len(), 1, "the second box is the first again");
        assert_eq!(
            d[0]["box"],
            serde_json::json!({"x1": 80.0, "y1": 15.0, "x2": 120.0, "y2": 35.0})
        );
        assert_eq!(v["image"], serde_json::json!({"width": 200, "height": 50}));
        assert_eq!(d[0]["label"], "class 0", "6 attributes are not COCO's 84");
        let labelled = Sidecar::parse(r#"{"labels": ["thing"]}"#).unwrap();
        let v = build_result(&dp, &labelled, &req(None, Some(0.85)), (&dims, &data), (200, 50)).unwrap();
        assert_eq!(v["detections"][0]["label"], "thing");
        let v = build_result(&dp, &labelled, &req(None, Some(0.95)), (&dims, &data), (200, 50)).unwrap();
        assert!(v["detections"].as_array().unwrap().is_empty());

        // embedding: normalised unless the sidecar says not; a token matrix gives its first row
        let ep = Plan {
            kind: Kind::Embed,
            output: 0,
            prep: prep(8, 8),
        };
        let v = build_result(&ep, &none, &req(None, None), (&[1, 2], &[3.0, 4.0]), (1, 1)).unwrap();
        assert_eq!(v["dimensions"], 2);
        assert!((v["embedding"][1].as_f64().unwrap() - 0.8).abs() < 1e-6);
        let raw = Sidecar::parse(r#"{"normalize": false}"#).unwrap();
        let v = build_result(&ep, &raw, &req(None, None), (&[1, 2], &[3.0, 4.0]), (1, 1)).unwrap();
        assert_eq!(v["embedding"], serde_json::json!([3.0, 4.0]));
        let v = build_result(&ep, &raw, &req(None, None), (&[1, 2, 2], &[1.0, 2.0, 7.0, 8.0]), (1, 1)).unwrap();
        assert_eq!(v["embedding"], serde_json::json!([1.0, 2.0]));
        assert!(build_result(&ep, &raw, &req(None, None), (&[2, 2, 2], &[0.0; 8]), (1, 1)).is_err());
    }

    #[test]
    fn the_cli_text_view() {
        let c = serde_json::json!({"kind": "classify", "predictions": [{"label": "cat", "score": 0.9123}]});
        assert_eq!(
            render_text(&c),
            " 91.23%  cat
"
        );
        let d = serde_json::json!({"kind": "detect", "detections": [
            {"label": "car", "score": 0.5, "box": {"x1": 1.0, "y1": 2.0, "x2": 3.0, "y2": 4.0}}]});
        assert_eq!(
            render_text(&d),
            " 50.00%  car  [1.0, 2.0, 3.0, 4.0]
"
        );
        assert_eq!(
            render_text(&serde_json::json!({"kind": "detect", "detections": []})),
            "nothing found
"
        );
        assert_eq!(
            render_text(&serde_json::json!({"kind": "embed", "embedding": [0.5, 1.0]})),
            "2 dimensions: [0.5000, 1.0000] (--json for the whole vector)
"
        );
    }

    #[test]
    fn coco_names_apply_to_80_class_detectors() {
        assert_eq!(COCO_LABELS[0], "person");
        assert_eq!(COCO_LABELS[79], "toothbrush");
        assert!(detector_has_80(&[1, 84, 8400]) && detector_has_80(&[1, 25200, 85]));
        assert!(!detector_has_80(&[1, 6, 20]) && !detector_has_80(&[1, 20, 6]));
        let dp = Plan {
            kind: Kind::Detect,
            output: 0,
            prep: prep(10, 10),
        };
        let mut scores = [0.0f32; 80];
        scores[2] = 0.9;
        let (dims, data) = v8_output(100, 80, &[(2, 5.0, 5.0, 4.0, 4.0, &scores)]);
        let v = build_result(&dp, &Sidecar::default(), &req(None, None), (&dims, &data), (10, 10)).unwrap();
        assert_eq!(v["detections"][0]["label"], "car");
    }

    #[cfg(not(feature = "vision"))]
    #[test]
    fn without_the_feature_analysis_is_501() {
        let e = analyze(Path::new("x.onnx"), &req(None, None), None).unwrap_err();
        assert_eq!(e.status, 501);
        assert!(e.message.contains("--features vision"));
    }

    #[cfg(feature = "vision")]
    mod session {
        use super::*;
        use crate::vision::test_model::color_share_model;

        #[test]
        fn a_hand_written_model_classifies() {
            let dir = tmp();
            std::fs::write(dir.join("shares.onnx"), color_share_model()).unwrap();
            std::fs::write(
                dir.join("shares.json"),
                r#"{"labels": ["red", "green", "blue"], "mean": [0, 0, 0], "std": [1, 1, 1], "scale": 0.01}"#,
            )
            .unwrap();
            // a green PNG, built with the image crate
            let mut png = Vec::new();
            image::RgbImage::from_pixel(9, 5, image::Rgb([10, 250, 10]))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            let body = serde_json::json!({"image": crate::image::base64(&png), "top_k": 2});
            let r = VisionRequest::from_json(&body, false).unwrap();
            let model = find_model(std::slice::from_ref(&dir), "shares").unwrap().1;
            let v = analyze(&model, &r, Some(Kind::Classify)).unwrap();
            assert_eq!(v["model"], "shares");
            assert_eq!(v["predictions"][0]["label"], "green");
            assert_eq!(v["predictions"].as_array().unwrap().len(), 2);
            assert!(v["predictions"][0]["score"].as_f64().unwrap() > 0.8, "{v}");
            // the same model refuses the other routes, and a bad picture is a 400
            assert_eq!(analyze(&model, &r, Some(Kind::Detect)).unwrap_err().status, 400);
            let junk = VisionRequest {
                image: b"not an image".to_vec(),
                ..r
            };
            assert_eq!(analyze(&model, &junk, None).unwrap_err().status, 400);
            // a model that is not ONNX fails with a clear 500
            std::fs::write(dir.join("broken.onnx"), b"junk").unwrap();
            let e = analyze(&dir.join("broken.onnx"), &junk_free(), None).unwrap_err();
            assert_eq!(e.status, 500, "{e:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        fn junk_free() -> VisionRequest {
            let mut png = Vec::new();
            image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            VisionRequest {
                model: String::new(),
                image: png,
                top_k: None,
                threshold: None,
            }
        }

        #[test]
        fn pictures_decode_and_oversized_ones_are_refused() {
            let mut png = Vec::new();
            image::RgbImage::from_pixel(3, 2, image::Rgb([1, 2, 3]))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            let (w, h, rgb) = decode_image(&png).unwrap();
            assert_eq!((w, h, rgb.len()), (3, 2, 18));
            assert_eq!(&rgb[..3], &[1, 2, 3]);
            let mut jpg = Vec::new();
            image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]))
                .write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
                .unwrap();
            assert_eq!(decode_image(&jpg).map(|d| (d.0, d.1)).unwrap(), (8, 8));
            assert_eq!(decode_image(b"GIF89a....").unwrap_err().status, 400);
            assert_eq!(decode_image(&[]).unwrap_err().status, 400);
        }
    }
}

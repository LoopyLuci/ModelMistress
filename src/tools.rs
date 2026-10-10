//! The llama.cpp tools as operations: request parsing, validation and argument building for `quantize`, `lora`,
//! `speculative` (a draft model), `embeddings` and `rerank`. Nothing here starts a process; the hub does that with
//! the pieces in `engine`, so all of it can be tested without a model.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The quantization types `llama-quantize` takes (its own list; matched without regard to case).
pub const QUANT_TYPES: &[&str] = &[
    "Q1_0",
    "Q2_0",
    "Q4_0",
    "Q4_1",
    "MXFP4_MOE",
    "Q5_0",
    "Q5_1",
    "IQ2_XXS",
    "IQ2_XS",
    "IQ2_S",
    "IQ2_M",
    "IQ1_S",
    "IQ1_M",
    "TQ1_0",
    "TQ2_0",
    "Q2_K",
    "Q2_K_S",
    "IQ3_XXS",
    "IQ3_S",
    "IQ3_M",
    "Q3_K",
    "IQ3_XS",
    "Q3_K_S",
    "Q3_K_M",
    "Q3_K_L",
    "IQ4_NL",
    "IQ4_XS",
    "Q4_K",
    "Q4_K_S",
    "Q4_K_M",
    "Q5_K",
    "Q5_K_S",
    "Q5_K_M",
    "Q6_K",
    "Q8_0",
    "F16",
    "BF16",
    "F32",
    "COPY",
];

fn opt_str(v: &Value, k: &str) -> std::result::Result<Option<String>, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("{k} must be a string")),
    }
}

fn opt_bool(v: &Value, k: &str) -> std::result::Result<bool, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{k} must be true or false")),
    }
}

fn opt_count(v: &Value, k: &str, max: u64) -> std::result::Result<Option<u32>, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_u64()
            .filter(|n| *n <= max)
            .map(|n| Some(n as u32))
            .ok_or_else(|| format!("{k} must be an integer from 0 to {max}")),
    }
}

fn strings(v: &Value, k: &str) -> std::result::Result<Vec<String>, String> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x.as_str().map(str::trim) {
                Some(s) if !s.is_empty() => Ok(s.to_string()),
                _ => Err(format!("{k} must be a list of non-empty strings")),
            })
            .collect(),
        Some(_) => Err(format!("{k} must be a list of strings")),
    }
}

// ---- quantize -----------------------------------------------------------------------------------------------

/// The canonical (upper case) name of a quantization type, or None.
pub fn quant_type(s: &str) -> Option<&'static str> {
    QUANT_TYPES.iter().copied().find(|t| t.eq_ignore_ascii_case(s.trim()))
}

/// A ggml tensor type name such as `q8_0` or `f16` (for the output-tensor and embedding overrides).
fn tensor_type(s: &str) -> std::result::Result<String, String> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(s.to_string())
    } else {
        Err(format!("\"{s}\" is not a tensor type name (such as q8_0 or f16)"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantizeRequest {
    /// A model on this machine (its id), or...
    pub model: Option<String>,
    /// ...a GGUF file's path.
    pub input: Option<PathBuf>,
    /// Where the quantized file goes; unset: `<home>/models/<name>-<TYPE>.gguf`.
    pub output: Option<PathBuf>,
    pub quant: &'static str,
    pub allow_requantize: bool,
    pub leave_output_tensor: bool,
    pub pure: bool,
    pub imatrix: Option<PathBuf>,
    pub include_weights: Vec<String>,
    pub exclude_weights: Vec<String>,
    pub output_tensor_type: Option<String>,
    pub token_embedding_type: Option<String>,
    pub threads: Option<u32>,
    pub overwrite: bool,
    pub wait: bool,
}

impl QuantizeRequest {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        if !v.is_object() {
            return Err("the input must be a JSON object".into());
        }
        let (model, input) = (opt_str(v, "model")?, opt_str(v, "input")?.map(PathBuf::from));
        if model.is_some() == input.is_some() {
            return Err("give either model (a model id from model.list) or input (the path of a .gguf file)".into());
        }
        let quant = opt_str(v, "type")?.ok_or("type is required, for example Q4_K_M")?;
        let quant = quant_type(&quant).ok_or_else(|| {
            format!(
                "unknown quantization type \"{quant}\"; use one of {}",
                QUANT_TYPES.join(", ")
            )
        })?;
        Ok(Self {
            model,
            input,
            output: opt_str(v, "output")?.map(PathBuf::from),
            quant,
            allow_requantize: opt_bool(v, "allow_requantize")?,
            leave_output_tensor: opt_bool(v, "leave_output_tensor")?,
            pure: opt_bool(v, "pure")?,
            imatrix: opt_str(v, "imatrix")?.map(PathBuf::from),
            include_weights: strings(v, "include_weights")?,
            exclude_weights: strings(v, "exclude_weights")?,
            output_tensor_type: opt_str(v, "output_tensor_type")?.map(|t| tensor_type(&t)).transpose()?,
            token_embedding_type: opt_str(v, "token_embedding_type")?
                .map(|t| tensor_type(&t))
                .transpose()?,
            threads: opt_count(v, "threads", 1024)?.filter(|n| *n > 0),
            overwrite: opt_bool(v, "overwrite")?,
            wait: opt_bool(v, "wait")?,
        })
    }

    /// `llama-quantize [options] input output type [threads]`
    pub fn tool_args(&self, input: &Path, output: &Path) -> Vec<String> {
        let mut a: Vec<String> = vec![];
        for (on, flag) in [
            (self.allow_requantize, "--allow-requantize"),
            (self.leave_output_tensor, "--leave-output-tensor"),
            (self.pure, "--pure"),
        ] {
            if on {
                a.push(flag.into());
            }
        }
        if let Some(i) = &self.imatrix {
            a.extend(["--imatrix".into(), i.display().to_string()]);
        }
        for w in &self.include_weights {
            a.extend(["--include-weights".into(), w.clone()]);
        }
        for w in &self.exclude_weights {
            a.extend(["--exclude-weights".into(), w.clone()]);
        }
        if let Some(t) = &self.output_tensor_type {
            a.extend(["--output-tensor-type".into(), t.clone()]);
        }
        if let Some(t) = &self.token_embedding_type {
            a.extend(["--token-embedding-type".into(), t.clone()]);
        }
        a.extend([
            input.display().to_string(),
            output.display().to_string(),
            self.quant.to_string(),
        ]);
        if let Some(n) = self.threads {
            a.push(n.to_string());
        }
        a
    }
}

/// `<dir>/<name>-<TYPE>.gguf` for a quantized copy of `input`: a type or part number already in the name
/// (`model-F16.gguf`, `model-00001-of-00003.gguf`) is replaced, not stacked.
pub fn default_output(input: &Path, dir: &Path, quant: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut base = stem.as_str();
    // "-00001-of-00003"
    if let Some(i) = base.rfind("-of-") {
        let (head, tail) = (&base[..i], &base[i + 4..]);
        if let Some(j) = head.rfind('-') {
            if tail.len() == 5
                && tail.bytes().all(|b| b.is_ascii_digit())
                && head[j + 1..].bytes().all(|b| b.is_ascii_digit())
            {
                base = &head[..j];
            }
        }
    }
    if let Some(i) = base.rfind(['-', '.']) {
        if quant_type(&base[i + 1..]).is_some() {
            base = &base[..i];
        }
    }
    dir.join(format!("{}-{quant}.gguf", if base.is_empty() { "model" } else { base }))
}

/// `[  12/ 291] blk.0.attn_k.weight ...` is llama-quantize's progress line: (12, 291).
pub fn quantize_progress(line: &str) -> Option<(u64, u64)> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (inside, _) = rest.split_once(']')?;
    let (done, total) = inside.split_once('/')?;
    let (done, total) = (done.trim().parse().ok()?, total.trim().parse().ok()?);
    (total > 0 && done <= total).then_some((done, total))
}

// ---- LoRA ---------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum LoraCmd {
    List,
    Attach {
        adapter: String,
        scale: f32,
    },
    /// One adapter, or all of them.
    Detach {
        adapter: Option<String>,
    },
    Scale {
        adapter: String,
        scale: f32,
    },
}

fn scale_of(v: &Value) -> std::result::Result<Option<f32>, String> {
    match v.get("scale") {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_f64()
            .filter(|s| s.is_finite() && s.abs() <= 100.0)
            .map(|s| Some(s as f32))
            .ok_or_else(|| "scale must be a number between -100 and 100".into()),
    }
}

impl LoraCmd {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        let action = opt_str(v, "action")?.unwrap_or_else(|| "list".into()).to_lowercase();
        let adapter = opt_str(v, "adapter")?;
        let need = |a: Option<String>| a.ok_or_else(|| format!("{action} needs adapter (a .gguf LoRA file)"));
        Ok(match action.as_str() {
            "list" => Self::List,
            "attach" => Self::Attach {
                adapter: need(adapter)?,
                scale: scale_of(v)?.unwrap_or(1.0),
            },
            "detach" => Self::Detach { adapter },
            "scale" => Self::Scale {
                adapter: need(adapter)?,
                scale: scale_of(v)?.ok_or("scale needs scale")?,
            },
            other => return Err(format!("unknown action \"{other}\"; use list, attach, detach or scale")),
        })
    }
}

// ---- speculative decoding -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum DraftMode {
    /// Say what is set now.
    Show,
    Off,
    /// A model id or the path of a .gguf file.
    Set(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftCmd {
    pub mode: DraftMode,
    pub n_max: Option<u32>,
    pub n_min: Option<u32>,
    pub gpu_layers: Option<i32>,
}

impl DraftCmd {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        let gpu_layers = match v.get("gpu_layers") {
            None | Some(Value::Null) => None,
            Some(x) => Some(
                x.as_i64()
                    .filter(|n| (-1..=999).contains(n))
                    .ok_or("gpu_layers must be an integer from -1 (auto) to 999")? as i32,
            ),
        };
        let (n_max, n_min) = (opt_count(v, "n_max", 4096)?, opt_count(v, "n_min", 4096)?);
        if let (Some(max), Some(min)) = (n_max, n_min) {
            if min > max {
                return Err("n_min is more than n_max".into());
            }
        }
        let mode = match (opt_str(v, "draft_model")?, opt_bool(v, "off")?) {
            (Some(_), true) => return Err("give draft_model or off, not both".into()),
            (Some(m), false) => DraftMode::Set(m),
            (None, true) => DraftMode::Off,
            (None, false) => DraftMode::Show,
        };
        Ok(Self {
            mode,
            n_max,
            n_min,
            gpu_layers,
        })
    }
}

// ---- embeddings and rerank ----------------------------------------------------------------------------------

/// Checks an OpenAI `/v1/embeddings` body (the engine answers anything it accepts).
pub fn check_embeddings(v: &Value) -> std::result::Result<(), String> {
    let obj = v.as_object().ok_or("the body must be a JSON object")?;
    if !matches!(obj.get("model"), None | Some(Value::String(_))) {
        return Err("model must be a string".into());
    }
    let tokens = |x: &Value| {
        x.as_array()
            .is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_u64))
    };
    let ok = match obj.get("input") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => {
            !a.is_empty()
                && (a.iter().all(|x| x.as_str().is_some_and(|s| !s.trim().is_empty()))
                    || a.iter().all(Value::is_u64)
                    || a.iter().all(tokens))
        }
        _ => false,
    };
    if !ok {
        return Err("input must be a non-empty string, a list of strings, or token ids".into());
    }
    match obj.get("encoding_format").map(|e| e.as_str()) {
        None | Some(Some("float")) | Some(Some("base64")) => Ok(()),
        _ => Err("encoding_format must be \"float\" or \"base64\"".into()),
    }
}

/// Checks a rerank body (Jina and llama-server style) and returns it ready to send: `documents` as strings.
pub fn check_rerank(v: &Value) -> std::result::Result<Value, String> {
    let obj = v.as_object().ok_or("the body must be a JSON object")?;
    if !matches!(obj.get("model"), None | Some(Value::String(_))) {
        return Err("model must be a string".into());
    }
    let query = obj.get("query").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if query.is_empty() {
        return Err("query is required".into());
    }
    let docs = obj
        .get("documents")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or("documents must be a non-empty list")?;
    let docs: Vec<String> = docs
        .iter()
        .map(|d| {
            d.as_str()
                .or_else(|| d.get("text").and_then(Value::as_str))
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .ok_or("each document must be a non-empty string (or an object with a text field)")
        })
        .collect::<std::result::Result<_, _>>()?;
    let mut out = json!({"query": query, "documents": docs});
    for k in ["model", "top_n"] {
        if let Some(x) = obj.get(k).filter(|x| !x.is_null()) {
            if k == "top_n" && !x.as_u64().is_some_and(|n| n >= 1) {
                return Err("top_n must be an integer of at least 1".into());
            }
            out[k] = x.clone();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_requests() {
        let r = QuantizeRequest::from_json(&json!({"model": "llama3.2:1b", "type": "q4_k_m"})).unwrap();
        assert_eq!(
            (r.quant, r.model.as_deref(), r.input.is_none()),
            ("Q4_K_M", Some("llama3.2:1b"), true)
        );
        let r = QuantizeRequest::from_json(&json!({
            "input": "/m/a.gguf", "type": "Q8_0", "output": "/o/a.gguf", "pure": true, "allow_requantize": true,
            "leave_output_tensor": true, "imatrix": "/m/im.dat", "include_weights": ["ffn_down"],
            "exclude_weights": ["attn_q"], "output_tensor_type": "q8_0", "token_embedding_type": "f16", "threads": 4
        }))
        .unwrap();
        let args = r.tool_args(Path::new("/m/a.gguf"), Path::new("/o/a.gguf"));
        assert_eq!(
            args,
            [
                "--allow-requantize",
                "--leave-output-tensor",
                "--pure",
                "--imatrix",
                "/m/im.dat",
                "--include-weights",
                "ffn_down",
                "--exclude-weights",
                "attn_q",
                "--output-tensor-type",
                "q8_0",
                "--token-embedding-type",
                "f16",
                "/m/a.gguf",
                "/o/a.gguf",
                "Q8_0",
                "4"
            ]
        );
        let plain = QuantizeRequest::from_json(&json!({"input": "a.gguf", "type": "Q4_0"})).unwrap();
        assert_eq!(
            plain.tool_args(Path::new("a.gguf"), Path::new("b.gguf")),
            ["a.gguf", "b.gguf", "Q4_0"]
        );
    }

    #[test]
    fn bad_quantize_requests() {
        let bad = |v: Value| QuantizeRequest::from_json(&v).unwrap_err();
        assert!(bad(json!({"type": "Q4_0"})).contains("either model"));
        assert!(bad(json!({"model": "a", "input": "b", "type": "Q4_0"})).contains("either model"));
        assert!(bad(json!({"model": "a"})).contains("type is required"));
        let e = bad(json!({"model": "a", "type": "Q9_9"}));
        assert!(e.contains("Q9_9") && e.contains("Q4_K_M"), "{e}");
        assert!(bad(json!({"model": "a", "type": "Q4_0", "pure": "yes"})).contains("pure"));
        assert!(bad(json!({"model": "a", "type": "Q4_0", "output_tensor_type": "q8 0; rm"})).contains("tensor type"));
        assert!(bad(json!({"model": "a", "type": "Q4_0", "include_weights": [""]})).contains("include_weights"));
        assert!(bad(json!({"model": "a", "type": "Q4_0", "threads": -1})).contains("threads"));
        assert!(bad(json!("x")).contains("object"));
    }

    #[test]
    fn output_names() {
        let d = Path::new("/out");
        let name = |i: &str, q: &str| {
            default_output(Path::new(i), d, q)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name("/m/llama-3.2-1b.gguf", "Q4_K_M"), "llama-3.2-1b-Q4_K_M.gguf");
        assert_eq!(name("/m/llama-F16.gguf", "Q4_K_M"), "llama-Q4_K_M.gguf");
        assert_eq!(name("/m/llama.bf16.gguf", "Q8_0"), "llama-Q8_0.gguf");
        assert_eq!(name("/m/big-00001-of-00003.gguf", "Q4_0"), "big-Q4_0.gguf");
        assert_eq!(name("/m/model-q8_0.gguf", "Q4_0"), "model-Q4_0.gguf");
        assert_eq!(default_output(Path::new("/m/x.gguf"), d, "F16").parent(), Some(d));
    }

    #[test]
    fn progress_lines() {
        let l = "[  12/ 291]                blk.0.attn_k.weight - [ 2048,  256,     1,     1], type =    f16, converting to q4_K .. size =     1.00 MiB ->     0.28 MiB";
        assert_eq!(quantize_progress(l), Some((12, 291)));
        assert_eq!(quantize_progress("[1/2] x"), Some((1, 2)));
        assert_eq!(quantize_progress("main: quantizing ..."), None);
        assert_eq!(quantize_progress("[ 5/ 0] x"), None);
        assert_eq!(quantize_progress("[ a/ b] x"), None);
        assert_eq!(quantize_progress("[INFO] fine"), None);
    }

    #[test]
    fn lora_commands() {
        let p = |v: Value| LoraCmd::from_json(&v);
        assert_eq!(p(json!({})).unwrap(), LoraCmd::List);
        assert_eq!(
            p(json!({"action": "attach", "adapter": "a.gguf"})).unwrap(),
            LoraCmd::Attach {
                adapter: "a.gguf".into(),
                scale: 1.0
            }
        );
        assert_eq!(
            p(json!({"action": "ATTACH", "adapter": "a.gguf", "scale": 0.5})).unwrap(),
            LoraCmd::Attach {
                adapter: "a.gguf".into(),
                scale: 0.5
            }
        );
        assert_eq!(
            p(json!({"action": "detach"})).unwrap(),
            LoraCmd::Detach { adapter: None }
        );
        assert_eq!(
            p(json!({"action": "scale", "adapter": "a", "scale": 0})).unwrap(),
            LoraCmd::Scale {
                adapter: "a".into(),
                scale: 0.0
            }
        );
        assert!(p(json!({"action": "attach"})).unwrap_err().contains("adapter"));
        assert!(p(json!({"action": "scale", "adapter": "a"}))
            .unwrap_err()
            .contains("scale"));
        assert!(p(json!({"action": "attach", "adapter": "a", "scale": 1000}))
            .unwrap_err()
            .contains("scale"));
        assert!(p(json!({"action": "merge"})).unwrap_err().contains("unknown action"));
    }

    #[test]
    fn draft_commands() {
        let d = DraftCmd::from_json(&json!({"draft_model": "qwen3:0.6b", "n_max": 16, "n_min": 2, "gpu_layers": 99}))
            .unwrap();
        assert_eq!(
            d,
            DraftCmd {
                mode: DraftMode::Set("qwen3:0.6b".into()),
                n_max: Some(16),
                n_min: Some(2),
                gpu_layers: Some(99)
            }
        );
        assert_eq!(DraftCmd::from_json(&json!({})).unwrap().mode, DraftMode::Show);
        assert_eq!(DraftCmd::from_json(&json!({"off": true})).unwrap().mode, DraftMode::Off);
        assert!(DraftCmd::from_json(&json!({"off": true, "draft_model": "x"}))
            .unwrap_err()
            .contains("not both"));
        assert!(DraftCmd::from_json(&json!({"n_max": 2, "n_min": 5}))
            .unwrap_err()
            .contains("n_min"));
        assert!(DraftCmd::from_json(&json!({"n_max": -1}))
            .unwrap_err()
            .contains("n_max"));
        assert!(DraftCmd::from_json(&json!({"gpu_layers": "all"}))
            .unwrap_err()
            .contains("gpu_layers"));
        assert!(DraftCmd::from_json(&json!({"draft_model": 3}))
            .unwrap_err()
            .contains("draft_model"));
    }

    #[test]
    fn embeddings_bodies() {
        assert!(check_embeddings(&json!({"model": "m", "input": "hello"})).is_ok());
        assert!(check_embeddings(&json!({"input": ["a", "b"]})).is_ok());
        assert!(check_embeddings(&json!({"input": [1, 2, 3]})).is_ok());
        assert!(check_embeddings(&json!({"input": [[1, 2], [3]], "encoding_format": "base64"})).is_ok());
        for bad in [
            json!({}),
            json!({"input": ""}),
            json!({"input": []}),
            json!({"input": ["a", 1]}),
            json!({"input": 5}),
            json!({"input": "x", "model": 1}),
            json!({"input": "x", "encoding_format": "hex"}),
            json!("x"),
        ] {
            assert!(check_embeddings(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rerank_bodies() {
        let ok = check_rerank(&json!({"model": "r", "query": " cats ", "documents": ["a", {"text": "b"}], "top_n": 1}))
            .unwrap();
        assert_eq!(
            ok,
            json!({"query": "cats", "documents": ["a", "b"], "model": "r", "top_n": 1})
        );
        assert_eq!(
            check_rerank(&json!({"query": "q", "documents": ["a"]})).unwrap(),
            json!({"query": "q", "documents": ["a"]})
        );
        for bad in [
            json!({"documents": ["a"]}),
            json!({"query": "q"}),
            json!({"query": "q", "documents": []}),
            json!({"query": "q", "documents": [""]}),
            json!({"query": "q", "documents": [1]}),
            json!({"query": "q", "documents": ["a"], "top_n": 0}),
            json!({"query": "q", "documents": ["a"], "model": 3}),
            json!([]),
        ] {
            assert!(check_rerank(&bad).is_err(), "{bad}");
        }
    }
}

//! Which engine runs a model, by format (see docs/native/km-factory.md section 1):
//!
//! | format | engine |
//! |---|---|
//! | GGUF | the bundled llama.cpp |
//! | Hugging Face safetensors / PyTorch | llama.cpp after a conversion, when llama.cpp knows the architecture; otherwise the Forge worker (PyTorch) |
//! | ONNX | ONNX Runtime |
//!
//! `plan` only decides; `forge.load` (mod.rs) acts on the plan and records where the model runs.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gguf,
    Safetensors,
    Pytorch,
    Onnx,
    Unknown,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Safetensors => "safetensors",
            Self::Pytorch => "pytorch",
            Self::Onnx => "onnx",
            Self::Unknown => "unknown",
        }
    }
}

/// The format of a file or a model folder.
pub fn detect_format(p: &Path) -> Format {
    let ext = |p: &Path| p.extension().and_then(|e| e.to_str()).map(str::to_lowercase);
    if p.is_file() {
        return match ext(p).as_deref() {
            Some("gguf") => Format::Gguf,
            Some("safetensors") => Format::Safetensors,
            Some("bin") | Some("pt") | Some("pth") => Format::Pytorch,
            Some("onnx") => Format::Onnx,
            _ => Format::Unknown,
        };
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return Format::Unknown;
    };
    let names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    let has = |f: &dyn Fn(&str) -> bool| names.iter().any(|n| f(n));
    if has(&|n| n.ends_with(".gguf")) && !has(&|n| n.ends_with(".safetensors")) {
        Format::Gguf
    } else if has(&|n| n.ends_with(".safetensors")) {
        Format::Safetensors
    } else if has(&|n| n.starts_with("pytorch_model") && n.ends_with(".bin")) {
        Format::Pytorch
    } else if has(&|n| n.ends_with(".onnx")) {
        Format::Onnx
    } else {
        Format::Unknown
    }
}

/// The architectures (Hugging Face `architectures[0]`) that llama.cpp's converter knows, read from its source
/// (`conversion/__init__.py`, a table of `"Arch": "module"` lines). None if the source is not there.
pub fn convertible_architectures(llama_cpp_src: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(llama_cpp_src.join("conversion").join("__init__.py")).ok()?;
    let mut out = vec![];
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix('"') {
            if let Some((arch, tail)) = rest.split_once("\":") {
                if !arch.is_empty()
                    && arch.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && tail.trim_start().starts_with('"')
                {
                    out.push(arch.to_string());
                }
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `architectures[0]` of a model folder's config.json.
pub fn architecture(dir: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).ok()?).ok()?;
    v["architectures"][0].as_str().map(String::from)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub format: Format,
    /// llama.cpp | llama.cpp (after conversion) | forge | onnxruntime | none
    pub engine: &'static str,
    pub architecture: Option<String>,
    pub needs_conversion: bool,
    pub reason: String,
}

impl Plan {
    pub fn to_json(&self) -> Value {
        json!({"format": self.format.name(), "engine": self.engine, "architecture": self.architecture,
               "needs_conversion": self.needs_conversion, "reason": self.reason})
    }
}

/// Decides the engine. `convertible` is what llama.cpp's converter supports (None: unknown, so the Forge worker).
pub fn plan(path: &Path, convertible: Option<&[String]>) -> Plan {
    let format = detect_format(path);
    let arch = if path.is_dir() { architecture(path) } else { None };
    let (engine, needs_conversion, reason) = match format {
        Format::Gguf => ("llama.cpp", false, "GGUF runs on the bundled llama.cpp".to_string()),
        Format::Safetensors | Format::Pytorch => match (&arch, convertible) {
            (Some(a), Some(list)) if list.iter().any(|x| x == a) => (
                "llama.cpp (after conversion)",
                true,
                format!("llama.cpp knows {a}: convert to GGUF once (cached), then it runs there"),
            ),
            (Some(a), _) => (
                "forge",
                false,
                format!("llama.cpp does not (or cannot be shown to) know {a}: the Forge worker runs it with PyTorch"),
            ),
            (None, _) => (
                "forge",
                false,
                "no config.json architecture: the Forge worker tries it with PyTorch".to_string(),
            ),
        },
        Format::Onnx => ("onnxruntime", false, "ONNX runs on ONNX Runtime".to_string()),
        Format::Unknown => ("none", false, "the format is not recognised".to_string()),
    };
    Plan {
        format,
        engine,
        architecture: arch,
        needs_conversion,
        reason,
    }
}

/// Where llama.cpp's source tree is, if anywhere (for the architecture list).
pub fn llama_cpp_src(forge_src: Option<&Path>) -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = vec![];
    if let Some(p) = std::env::var_os("MM_FORGE_LLAMA_CPP") {
        c.push(p.into());
    }
    if let Some(s) = forge_src {
        c.push(s.join("..").join("engines").join("llama.cpp"));
    }
    c.into_iter()
        .find(|p| p.join("conversion").join("__init__.py").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("mm-forge-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn formats_are_told_apart_by_file_and_folder() {
        let d = dir();
        for (f, want) in [
            ("a.gguf", Format::Gguf),
            ("a.safetensors", Format::Safetensors),
            ("a.onnx", Format::Onnx),
            ("a.bin", Format::Pytorch),
            ("a.txt", Format::Unknown),
        ] {
            std::fs::write(d.join(f), b"x").unwrap();
            assert_eq!(detect_format(&d.join(f)), want, "{f}");
        }
        let hf = d.join("hf");
        std::fs::create_dir_all(&hf).unwrap();
        std::fs::write(hf.join("model-00001-of-00002.safetensors"), b"x").unwrap();
        assert_eq!(detect_format(&hf), Format::Safetensors);
        let pt = d.join("pt");
        std::fs::create_dir_all(&pt).unwrap();
        std::fs::write(pt.join("pytorch_model.bin"), b"x").unwrap();
        assert_eq!(detect_format(&pt), Format::Pytorch);
        let g = d.join("g");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("m-Q4.gguf"), b"x").unwrap();
        assert_eq!(detect_format(&g), Format::Gguf);
        assert_eq!(detect_format(&d.join("missing")), Format::Unknown);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_converter_table_is_read_from_llama_cpp_source() {
        let d = dir();
        std::fs::create_dir_all(d.join("conversion")).unwrap();
        std::fs::write(
            d.join("conversion").join("__init__.py"),
            "X = 1\nMODEL_MODULES = {\n    \"LlamaForCausalLM\": \"llama\",\n    \"Qwen3_5MoeForCausalLM\": \"qwen\",\n}\nOTHER = \"not an arch\"\n",
        )
        .unwrap();
        assert_eq!(
            convertible_architectures(&d).unwrap(),
            ["LlamaForCausalLM", "Qwen3_5MoeForCausalLM"]
        );
        assert!(convertible_architectures(&d.join("none")).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_plan_picks_the_engine_by_format_and_architecture() {
        let d = dir();
        let model = d.join("m");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("w.safetensors"), b"x").unwrap();
        std::fs::write(model.join("config.json"), r#"{"architectures":["LlamaForCausalLM"]}"#).unwrap();
        let known = vec!["LlamaForCausalLM".to_string()];
        let p = plan(&model, Some(&known));
        assert_eq!((p.engine, p.needs_conversion), ("llama.cpp (after conversion)", true));
        std::fs::write(
            model.join("config.json"),
            r#"{"architectures":["FancyNewForCausalLM"]}"#,
        )
        .unwrap();
        let p = plan(&model, Some(&known));
        assert_eq!((p.engine, p.needs_conversion), ("forge", false));
        assert!(p.reason.contains("FancyNewForCausalLM"));
        assert_eq!(plan(&model, None).engine, "forge", "unknown support means PyTorch");
        std::fs::write(d.join("x.gguf"), b"x").unwrap();
        assert_eq!(plan(&d.join("x.gguf"), None).engine, "llama.cpp");
        std::fs::write(d.join("x.onnx"), b"x").unwrap();
        assert_eq!(plan(&d.join("x.onnx"), None).engine, "onnxruntime");
        assert_eq!(plan(&d.join("nothing"), None).engine, "none");
        let _ = std::fs::remove_dir_all(d);
    }
}

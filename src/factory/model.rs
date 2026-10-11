//! Models the factory uses: the traits other modules plug into (the Forge provides LoRA training and HF-model serving),
//! the default generator (any model the hub serves through llama.cpp, every call traced with the task and job), device
//! placement, and reading JSON out of what small models say.

use std::path::PathBuf;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::store::Res;
use crate::engine::{Fail, LoadOpts, Target};
use crate::hub::Hub;
use crate::trace::Scope;

/// Where a model runs: `auto` (the hub decides), `gpu`, `cpu`, or `layers N` (N layers on the GPU).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Device {
    #[default]
    Auto,
    Gpu,
    Cpu,
    Layers(i32),
}

impl Device {
    pub fn parse(s: &str) -> Result<Device, String> {
        let t = s.trim().to_lowercase();
        let t = t.replace(['_', '-'], " ");
        match t.as_str() {
            "" | "auto" => Ok(Device::Auto),
            "gpu" => Ok(Device::Gpu),
            "cpu" => Ok(Device::Cpu),
            _ => {
                let n = t
                    .strip_prefix("layers")
                    .or_else(|| t.strip_prefix("split:"))
                    .or_else(|| t.strip_prefix("split"))
                    .map(str::trim)
                    .and_then(|n| n.parse::<i32>().ok())
                    .filter(|n| *n >= 0);
                n.map(Device::Layers)
                    .ok_or_else(|| format!("device {s:?} must be auto, gpu, cpu or layers N"))
            }
        }
    }

    /// The `gpu_layers` the engine is loaded with; None leaves the hub's setting.
    pub fn gpu_layers(self) -> Option<i32> {
        match self {
            Device::Auto => None,
            Device::Gpu => Some(999),
            Device::Cpu => Some(0),
            Device::Layers(n) => Some(n),
        }
    }

    pub fn label(self) -> String {
        match self {
            Device::Auto => "auto".into(),
            Device::Gpu => "gpu".into(),
            Device::Cpu => "cpu".into(),
            Device::Layers(n) => format!("layers {n}"),
        }
    }
}

/// One model call.
#[derive(Debug, Clone)]
pub struct GenRequest {
    /// A hub model id.
    pub model: String,
    /// Chat messages (`{role, content}`).
    pub messages: Vec<Value>,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Ask for a JSON object.
    pub json: bool,
    /// Let a reasoning model think first (off by default: it spends the token budget).
    pub think: bool,
    pub device: Device,
    /// The job and task the call belongs to; the trace is filed under them.
    pub scope: Scope,
    /// What the call is for (`plan`, `generate`, `verify`...); stored in the trace's meta.
    pub purpose: String,
}

#[derive(Debug, Clone, Default)]
pub struct GenReply {
    pub text: String,
    pub reasoning: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub trace: Option<String>,
}

/// "Generate with model X". The default is [`HubGenerator`]; the Forge registers its own to serve Hugging Face models
/// the bundled llama.cpp cannot load.
pub trait Generate: Send + Sync {
    fn generate<'a>(&'a self, hub: &'a Hub, req: GenRequest) -> BoxFuture<'a, Res<GenReply>>;
}

/// "Train an adapter".
#[derive(Debug, Clone)]
pub struct TrainRequest {
    pub task: String,
    /// The adapter KM's id.
    pub km_id: String,
    /// The core the adapter is for.
    pub base: String,
    /// The data KM the records came from (`id@version`).
    pub data_km: String,
    /// The training records (JSONL, `km-spec.md` section 8): only approved, split `train`.
    pub records: PathBuf,
    /// Where the adapter file goes.
    pub out_dir: PathBuf,
    pub device: Device,
    pub scope: Scope,
}

#[derive(Debug, Clone)]
pub struct TrainOutcome {
    /// The LoRA file (GGUF).
    pub adapter: PathBuf,
    /// The core it is for (`dusty`, `misty`), as `compat.base` of the adapter KM.
    pub core: String,
    pub arch: Option<String>,
    pub base_hash: Option<String>,
    pub run_id: String,
    pub metrics: Value,
}

pub trait Trainer: Send + Sync {
    fn train<'a>(&'a self, hub: &'a Hub, req: TrainRequest) -> BoxFuture<'a, Res<TrainOutcome>>;
}

/// Models served by llama.cpp through the hub, traced.
pub struct HubGenerator;

impl Generate for HubGenerator {
    fn generate<'a>(&'a self, hub: &'a Hub, req: GenRequest) -> BoxFuture<'a, Res<GenReply>> {
        Box::pin(async move {
            // placement: the model is loaded where the task asks unless it is already loaded
            if let (Ok(Target::Llama(entry)), Some(layers)) = (hub.engine.resolve(&req.model), req.device.gpu_layers())
            {
                if !hub.engine.is_loaded(&entry.id) {
                    let opts = LoadOpts {
                        gpu_layers: Some(layers),
                        ..Default::default()
                    };
                    hub.engine.ensure_loaded(&entry, &opts, false).await?;
                }
            }
            let mut body = json!({"model": req.model, "messages": req.messages, "max_tokens": req.max_tokens,
                                  "temperature": req.temperature, "stream": false});
            if req.json {
                body["response_format"] = json!({"type": "json_object"});
            }
            if !req.think {
                body["chat_template_kwargs"] = json!({"enable_thinking": false});
            }
            let meta = json!({"source": "factory", "purpose": req.purpose, "device": req.device.label()});
            let (trace, v) = match crate::trace::capture::run_traced(hub, body.clone(), None, meta, &req.scope).await {
                Ok((id, v)) => (Some(id), v),
                // an Ollama model, or a trace store that will not open: the call is still made, untraced
                Err(f) if f.code == "untraceable" || f.code == "unavailable" && f.message.contains("trace") => {
                    let (_, resp, _lease) = hub.engine.forward("/v1/chat/completions", body).await?;
                    let status = resp.status();
                    let v: Value = resp.json().await.unwrap_or(Value::Null);
                    if !status.is_success() {
                        return Err(Fail::new(
                            502,
                            "backend_error",
                            v["error"]["message"]
                                .as_str()
                                .unwrap_or("the model refused the request")
                                .to_string(),
                        ));
                    }
                    (None, v)
                }
                Err(f) => return Err(f),
            };
            let msg = &v["choices"][0]["message"];
            let (text, reasoning) = split_think(
                msg["content"].as_str().unwrap_or(""),
                msg["reasoning_content"].as_str().unwrap_or(""),
            );
            Ok(GenReply {
                text,
                reasoning,
                prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                trace,
            })
        })
    }
}

/// Removes `<think>...</think>` from an answer (and keeps it as the reasoning).
pub fn split_think(content: &str, reasoning: &str) -> (String, String) {
    let mut text = content.to_string();
    let mut why = reasoning.to_string();
    if let Some(end) = text.find("</think>") {
        let before = text[..end].replace("<think>", "");
        if why.is_empty() {
            why = before.trim().to_string();
        }
        text = text[end + "</think>".len()..].to_string();
    } else if let Some(start) = text.find("<think>") {
        // never closed: the answer was cut off while thinking
        text = text[..start].to_string();
    }
    (text.trim().to_string(), why)
}

/// The first JSON object in a model's answer (it may be fenced, or surrounded by words).
pub fn extract_json(text: &str) -> Result<Value, String> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    let unfenced = if let Some(i) = t.find("```") {
        let rest = &t[i + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        rest.split("```").next().unwrap_or(rest).trim().to_string()
    } else {
        t.to_string()
    };
    if let Ok(v) = serde_json::from_str::<Value>(&unfenced) {
        return Ok(v);
    }
    // from the first brace to the last
    match (unfenced.find('{'), unfenced.rfind('}')) {
        (Some(a), Some(b)) if b > a => {
            serde_json::from_str::<Value>(&unfenced[a..=b]).map_err(|e| format!("not valid JSON: {e}"))
        }
        _ => Err("no JSON object in the answer".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_parse() {
        assert_eq!(Device::parse("auto").unwrap(), Device::Auto);
        assert_eq!(Device::parse("GPU").unwrap(), Device::Gpu);
        assert_eq!(Device::parse("cpu").unwrap().gpu_layers(), Some(0));
        assert_eq!(Device::parse("layers 12").unwrap(), Device::Layers(12));
        assert_eq!(Device::parse("split:7").unwrap(), Device::Layers(7));
        assert!(Device::parse("tpu").is_err() && Device::parse("layers x").is_err());
    }

    #[test]
    fn json_is_found_in_fences_and_prose() {
        assert_eq!(extract_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(extract_json("Sure!\n```json\n{\"a\": 2}\n```").unwrap()["a"], 2);
        assert_eq!(extract_json("here: {\"a\": 3} done").unwrap()["a"], 3);
        assert!(extract_json("nothing").is_err());
        assert!(extract_json("{\"a\": ").is_err());
    }

    #[test]
    fn thinking_is_removed() {
        assert_eq!(
            split_think("<think>hmm</think>\n\nYes", ""),
            ("Yes".into(), "hmm".into())
        );
        assert_eq!(split_think("hmm</think>Yes", ""), ("Yes".into(), "hmm".into()));
        assert_eq!(split_think("plain", "r"), ("plain".into(), "r".into()));
        assert_eq!(split_think("<think>cut", "").0, "");
    }
}

//! Ollama's server API on the hub, next to the OpenAI one: any Ollama client or tool can point at ModelMistress.
//!
//!   GET  /api/version                  {"version"}
//!   GET  /api/tags                     the servable models (and Modelfile presets), as Ollama lists them
//!   POST /api/show                     one model: modelfile, parameters, template, details, model_info
//!   POST /api/chat                     chat, NDJSON when streaming (the chat path behind /v1/chat/completions)
//!   POST /api/generate                 a prompt (raw or templated: the completions path; else a one-turn chat)
//!   POST /api/embed, /api/embeddings   embeddings (the path behind /v1/embeddings)
//!   GET  /api/ps                       the loaded models
//!   POST /api/pull                     download (status lines); DELETE /api/delete removes what MM downloaded
//!   POST /api/create                   register a Modelfile preset; POST /api/copy copies one
//!
//! The same token as `/v1` (Bearer); a peer key is refused here (the owner only). The engine, loading, idle unload
//! and the VRAM planner are the existing ones: these routes translate shapes and call them.
//!
//! A *preset* is MM's name for what a Modelfile makes: a base model plus a system prompt, default parameters, a
//! template, messages and LoRA adapters, kept in `<home>/presets.json`. Weights are never copied. Using the preset's
//! name in a request runs its base model with those defaults.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::engine::{Fail, Target};
use crate::hub::{self, ApiError, Hub};

// ---- presets and the Modelfile -------------------------------------------------------------------------------

/// What a Modelfile makes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// The base model (an MM model id).
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Default sampling parameters (`temperature`, `stop` as an array, ...).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub parameters: Map<String, Value>,
    /// LoRA adapters (.gguf paths), attached to the base model when the preset is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapters: Vec<String>,
    /// Example turns put before every chat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created: String,
}

const INT_PARAMS: &[&str] = &[
    "num_ctx",
    "num_predict",
    "top_k",
    "repeat_last_n",
    "seed",
    "num_gpu",
    "mirostat",
    "num_keep",
    "num_batch",
    "num_thread",
];
const FLOAT_PARAMS: &[&str] = &[
    "temperature",
    "top_p",
    "min_p",
    "repeat_penalty",
    "presence_penalty",
    "frequency_penalty",
    "mirostat_tau",
    "mirostat_eta",
    "typical_p",
    "tfs_z",
];

fn param_value(k: &str, raw: &str) -> Result<Value, String> {
    let bad = || format!("PARAMETER {k}: {raw:?} is not a valid value");
    if INT_PARAMS.contains(&k) {
        raw.parse::<i64>().map(Value::from).map_err(|_| bad())
    } else if FLOAT_PARAMS.contains(&k) {
        raw.parse::<f64>().map(Value::from).map_err(|_| bad())
    } else {
        Ok(json!(raw.trim_matches('"')))
    }
}

/// The value of an instruction: a `"""block"""` over several lines, a "quoted" string, or the rest of the line.
fn take_value(rest: &str, lines: &[&str], i: &mut usize) -> String {
    let rest = rest.trim();
    if let Some(body) = rest.strip_prefix("\"\"\"") {
        if let Some(b) = body.strip_suffix("\"\"\"") {
            return b.to_string();
        }
        let mut parts = vec![body.to_string()];
        while *i < lines.len() {
            let ln = lines[*i];
            *i += 1;
            if let Some(end) = ln.trim_end().strip_suffix("\"\"\"") {
                parts.push(end.to_string());
                break;
            }
            parts.push(ln.to_string());
        }
        return parts.join("\n");
    }
    if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
        return rest[1..rest.len() - 1].to_string();
    }
    rest.to_string()
}

/// Parse a Modelfile: FROM, SYSTEM, TEMPLATE, PARAMETER, ADAPTER, MESSAGE, LICENSE (REQUIRES is accepted, ignored).
pub fn parse_modelfile(text: &str) -> Result<Preset, String> {
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut p = Preset::default();
    let mut i = 0;
    let mut from = None;
    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        match cmd.to_uppercase().as_str() {
            "FROM" => from = Some(take_value(rest, &lines, &mut i)),
            "SYSTEM" => p.system = Some(take_value(rest, &lines, &mut i)),
            "TEMPLATE" => p.template = Some(take_value(rest, &lines, &mut i)),
            "LICENSE" => p.license = Some(take_value(rest, &lines, &mut i)),
            "ADAPTER" => p.adapters.push(take_value(rest, &lines, &mut i)),
            "PARAMETER" => {
                let (k, v) = rest
                    .trim()
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| format!("PARAMETER needs a name and a value: {line}"))?;
                if k == "stop" {
                    let s = json!(v.trim().trim_matches('"'));
                    match p.parameters.get_mut("stop") {
                        Some(Value::Array(a)) => a.push(s),
                        _ => {
                            p.parameters.insert("stop".into(), json!([s]));
                        }
                    }
                } else {
                    p.parameters.insert(k.to_string(), param_value(k, v.trim())?);
                }
            }
            "MESSAGE" => {
                let (role, content) = rest
                    .trim()
                    .split_once(char::is_whitespace)
                    .ok_or_else(|| format!("MESSAGE needs a role and a text: {line}"))?;
                if !["system", "user", "assistant"].contains(&role) {
                    return Err(format!("MESSAGE role {role:?} is not system, user or assistant"));
                }
                let content = take_value(content, &lines, &mut i);
                p.messages.push(json!({"role": role, "content": content}));
            }
            "REQUIRES" => {}
            other => return Err(format!("unknown Modelfile instruction {other}")),
        }
    }
    p.from = from.filter(|f| !f.is_empty()).ok_or("a Modelfile needs a FROM line")?;
    Ok(p)
}

fn block(s: &str) -> String {
    format!("\"\"\"{s}\"\"\"")
}

fn plain(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// The Modelfile of a preset (what `ollama show --modelfile` prints); `parse_modelfile` reads it back.
pub fn render_modelfile(p: &Preset) -> String {
    let mut out = format!("FROM {}\n", p.from);
    for a in &p.adapters {
        out.push_str(&format!("ADAPTER {a}\n"));
    }
    if let Some(t) = &p.template {
        out.push_str(&format!("TEMPLATE {}\n", block(t)));
    }
    if let Some(s) = &p.system {
        out.push_str(&format!("SYSTEM {}\n", block(s)));
    }
    for (k, v) in &p.parameters {
        for it in v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]) {
            out.push_str(&format!("PARAMETER {k} {}\n", plain(&it)));
        }
    }
    for m in &p.messages {
        out.push_str(&format!(
            "MESSAGE {} {}\n",
            m["role"].as_str().unwrap_or("user"),
            block(m["content"].as_str().unwrap_or(""))
        ));
    }
    if let Some(l) = &p.license {
        out.push_str(&format!("LICENSE {}\n", block(l)));
    }
    out
}

/// The name a preset is kept under: `name:latest` is `name`.
pub fn canon(name: &str) -> String {
    let n = name.trim();
    n.strip_suffix(":latest").unwrap_or(n).to_string()
}

static STORE: Mutex<()> = Mutex::new(());

fn store_path(home: &Path) -> std::path::PathBuf {
    home.join("presets.json")
}

pub fn load_presets(home: &Path) -> BTreeMap<String, Preset> {
    std::fs::read(store_path(home))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_presets(home: &Path, all: &BTreeMap<String, Preset>) -> Result<(), Fail> {
    let io = |e: std::io::Error| Fail::new(500, "io", format!("presets.json: {e}"));
    let path = store_path(home);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(all).unwrap_or_default()).map_err(io)?;
    std::fs::rename(&tmp, &path).map_err(io)
}

/// Change the preset store under one lock.
fn edit_presets<T>(home: &Path, f: impl FnOnce(&mut BTreeMap<String, Preset>) -> Result<T, Fail>) -> Result<T, Fail> {
    let _g = STORE.lock();
    let mut all = load_presets(home);
    let out = f(&mut all)?;
    save_presets(home, &all)?;
    Ok(out)
}

pub fn get_preset(home: &Path, name: &str) -> Option<Preset> {
    load_presets(home).remove(&canon(name))
}

// ---- shapes: requests and answers ------------------------------------------------------------------------------

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// Ollama's `keep_alive`: seconds, or "5m"/"1h"/"30s"; returns the seconds (negative = forever).
pub fn keep_alive_s(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as i64),
        Value::String(s) => {
            let s = s.trim();
            let (num, mult) = match s.chars().last()? {
                's' => (&s[..s.len() - 1], 1.0),
                'm' => (&s[..s.len() - 1], 60.0),
                'h' => (&s[..s.len() - 1], 3600.0),
                _ => (s, 1.0),
            };
            num.parse::<f64>().ok().map(|f| (f * mult) as i64)
        }
        _ => None,
    }
}

/// Ollama `options` (and a preset's parameters) as llama-server request fields.
pub fn apply_options(body: &mut Value, opts: &Map<String, Value>) {
    for (k, v) in opts {
        let key = match k.as_str() {
            "num_predict" => "max_tokens",
            "stop" | "temperature" | "top_p" | "top_k" | "min_p" | "seed" | "presence_penalty"
            | "frequency_penalty" | "repeat_penalty" | "repeat_last_n" | "mirostat" | "mirostat_tau"
            | "mirostat_eta" | "typical_p" => k.as_str(),
            _ => continue, // num_ctx, num_gpu...: load-time settings, not per-request
        };
        if key == "max_tokens" && v.as_i64().is_some_and(|n| n < 0) {
            continue; // -1 = until done
        }
        body[key] = v.clone();
    }
}

/// An Ollama message as an OpenAI one: `images` (base64) become image parts, tool-call arguments become strings.
pub fn openai_message(m: &Value) -> Value {
    let mut out = m.clone();
    if let Some(imgs) = m.get("images").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let mut parts = vec![json!({"type": "text", "text": m["content"].as_str().unwrap_or("")})];
        for im in imgs.iter().filter_map(Value::as_str) {
            let url = if im.starts_with("data:") {
                im.to_string()
            } else {
                format!("data:image/png;base64,{im}")
            };
            parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
        }
        out["content"] = json!(parts);
    }
    if let Some(o) = out.as_object_mut() {
        o.remove("images");
        o.remove("thinking");
        if let Some(name) = o.remove("tool_name") {
            o.insert("name".into(), name);
        }
    }
    if let Some(calls) = out.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (n, c) in calls.iter_mut().enumerate() {
            if c.get("type").is_none() {
                c["type"] = json!("function");
            }
            if c.get("id").is_none() {
                c["id"] = json!(format!("call_{n}"));
            }
            if let Some(a) = c["function"].get("arguments").filter(|a| !a.is_string()).cloned() {
                c["function"]["arguments"] = json!(a.to_string());
            }
        }
    }
    out
}

/// `/api/chat` request -> `/v1/chat/completions` body. `preset` supplies the system prompt, messages, parameters.
pub fn chat_body(req: &Value, model: &str, preset: Option<&Preset>) -> Value {
    let given: Vec<Value> = req["messages"].as_array().cloned().unwrap_or_default();
    let mut messages: Vec<Value> = vec![];
    if let Some(p) = preset {
        if !given.iter().any(|m| m["role"] == "system") {
            if let Some(s) = &p.system {
                messages.push(json!({"role": "system", "content": s}));
            }
        }
        messages.extend(p.messages.iter().cloned());
    }
    messages.extend(given.iter().map(openai_message));
    let mut body = json!({"model": model, "messages": messages});
    finish_body(&mut body, req, preset);
    body
}

/// The fields `/api/chat` and `/api/generate` share.
fn finish_body(body: &mut Value, req: &Value, preset: Option<&Preset>) {
    if let Some(p) = preset {
        apply_options(body, &p.parameters);
    }
    if let Some(o) = req.get("options").and_then(Value::as_object) {
        apply_options(body, o);
    }
    if let Some(t) = req.get("tools").filter(|t| t.as_array().is_some_and(|a| !a.is_empty())) {
        body["tools"] = t.clone();
    }
    match req.get("format") {
        Some(Value::String(f)) if f == "json" => body["response_format"] = json!({"type": "json_object"}),
        Some(schema @ Value::Object(_)) => {
            body["response_format"] =
                json!({"type": "json_schema", "json_schema": {"name": "output", "schema": schema}})
        }
        _ => {}
    }
    if streaming(req) {
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage": true});
    }
}

/// Ollama streams unless told not to.
pub fn streaming(req: &Value) -> bool {
    req.get("stream").and_then(Value::as_bool).unwrap_or(true)
}

/// A template with `{{ .System }}`, `{{ .Prompt }}` and (not nested) `{{ if .X }}..{{ end }}`.
pub fn render_template(t: &str, system: &str, prompt: &str) -> String {
    let var = |name: &str| match name {
        ".System" => system,
        ".Prompt" => prompt,
        _ => "",
    };
    let mut out = String::new();
    let mut rest = t;
    let mut skipping = false;
    while let Some(a) = rest.find("{{") {
        if !skipping {
            out.push_str(&rest[..a]);
        }
        let Some(b) = rest[a..].find("}}") else { break };
        let inner = rest[a + 2..a + b]
            .trim()
            .trim_start_matches('-')
            .trim_end_matches('-')
            .trim();
        rest = &rest[a + b + 2..];
        if let Some(cond) = inner.strip_prefix("if ") {
            skipping = skipping || var(cond.trim()).is_empty();
        } else if inner == "end" {
            skipping = false;
        } else if !skipping {
            out.push_str(var(inner));
        }
    }
    if !skipping {
        out.push_str(rest);
    }
    out
}

fn ns(ms: Option<f64>) -> u64 {
    (ms.unwrap_or(0.0) * 1e6) as u64
}

/// The counts and durations Ollama puts on a final answer, from OpenAI's `usage` and llama-server's `timings`.
fn metrics(usage: Option<&Value>, timings: Option<&Value>, total: Duration) -> Value {
    let n = |k: &str| usage.and_then(|u| u[k].as_u64()).unwrap_or(0);
    let f = |k: &str| timings.and_then(|t| t[k].as_f64());
    json!({"total_duration": total.as_nanos() as u64, "load_duration": 0,
           "prompt_eval_count": n("prompt_tokens"), "prompt_eval_duration": ns(f("prompt_ms")),
           "eval_count": n("completion_tokens"), "eval_duration": ns(f("predicted_ms"))})
}

fn done_reason(finish: Option<&str>) -> &'static str {
    if finish == Some("length") {
        "length"
    } else {
        "stop"
    }
}

fn extend(into: &mut Value, from: Value) {
    if let (Some(o), Value::Object(m)) = (into.as_object_mut(), from) {
        o.extend(m);
    }
}

/// Tool calls as Ollama has them: `arguments` is an object.
fn ollama_tool_calls(calls: &Value) -> Value {
    let out: Vec<Value> = calls
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let args = match c["function"]["arguments"].as_str() {
                Some(s) => serde_json::from_str(s).unwrap_or_else(|_| json!(s)),
                None => c["function"]["arguments"].clone(),
            };
            json!({"function": {"name": c["function"]["name"], "arguments": args}})
        })
        .collect();
    json!(out)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Chat,
    Generate,
}

/// A whole (not streamed) OpenAI answer as Ollama's.
pub fn answer(kind: Kind, model: &str, v: &Value, total: Duration) -> Value {
    let choice = &v["choices"][0];
    let msg = &choice["message"];
    let text = msg["content"].as_str().unwrap_or("");
    let mut out = match kind {
        Kind::Chat => {
            let mut m = json!({"role": "assistant", "content": text});
            if let Some(t) = msg["reasoning_content"].as_str().filter(|t| !t.is_empty()) {
                m["thinking"] = json!(t);
            }
            if msg["tool_calls"].as_array().is_some_and(|a| !a.is_empty()) {
                m["tool_calls"] = ollama_tool_calls(&msg["tool_calls"]);
            }
            json!({"model": model, "created_at": now(), "message": m, "done": true})
        }
        Kind::Generate => {
            // /v1/completions carries the text on the choice, the chat path on its message
            let t = choice["text"].as_str().unwrap_or(text);
            json!({"model": model, "created_at": now(), "response": t, "done": true})
        }
    };
    out["done_reason"] = json!(done_reason(choice["finish_reason"].as_str()));
    extend(&mut out, metrics(v.get("usage"), v.get("timings"), total));
    out
}

/// Turns llama-server's server-sent events into Ollama's NDJSON objects, whatever the chunking of the bytes.
pub struct Ndjson {
    kind: Kind,
    model: String,
    pending: Vec<u8>,
    finish: Option<String>,
    usage: Option<Value>,
    timings: Option<Value>,
    calls: BTreeMap<u64, (String, String)>,
    done: bool,
    began: Instant,
}

impl Ndjson {
    pub fn new(kind: Kind, model: &str) -> Self {
        Self {
            kind,
            model: model.to_string(),
            pending: vec![],
            finish: None,
            usage: None,
            timings: None,
            calls: BTreeMap::new(),
            done: false,
            began: Instant::now(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.pending.extend_from_slice(bytes);
        let mut out = vec![];
        while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=nl).collect();
            self.line(&String::from_utf8_lossy(&raw), &mut out);
        }
        out
    }

    /// The stream ended: the last line, if `[DONE]` did not already send it.
    pub fn finish(&mut self) -> Vec<Value> {
        let mut out = vec![];
        if !self.pending.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned();
            self.line(&rest, &mut out);
        }
        self.last(&mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<Value>) {
        let Some(data) = line.trim().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            return self.last(out);
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if v.get("usage").is_some_and(|u| !u.is_null()) {
            self.usage = v.get("usage").cloned();
        }
        if v.get("timings").is_some() {
            self.timings = v.get("timings").cloned();
        }
        let choice = &v["choices"][0];
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        let delta = &choice["delta"];
        for c in delta["tool_calls"].as_array().into_iter().flatten() {
            let slot = self.calls.entry(c["index"].as_u64().unwrap_or(0)).or_default();
            if let Some(n) = c["function"]["name"].as_str() {
                slot.0.push_str(n);
            }
            if let Some(a) = c["function"]["arguments"].as_str() {
                slot.1.push_str(a);
            }
        }
        let text = delta["content"]
            .as_str()
            .or_else(|| choice["text"].as_str())
            .unwrap_or("");
        let think = delta["reasoning_content"].as_str().unwrap_or("");
        if text.is_empty() && think.is_empty() {
            return;
        }
        out.push(match self.kind {
            Kind::Chat => {
                let mut m = json!({"role": "assistant", "content": text});
                if !think.is_empty() {
                    m["thinking"] = json!(think);
                }
                json!({"model": self.model, "created_at": now(), "message": m, "done": false})
            }
            Kind::Generate => json!({"model": self.model, "created_at": now(), "response": text, "done": false}),
        });
    }

    fn last(&mut self, out: &mut Vec<Value>) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        let mut v = match self.kind {
            Kind::Chat => {
                let mut m = json!({"role": "assistant", "content": ""});
                if !self.calls.is_empty() {
                    let calls: Vec<Value> = self
                        .calls
                        .values()
                        .map(|(n, a)| json!({"function": {"name": n, "arguments": a}}))
                        .collect();
                    m["tool_calls"] = ollama_tool_calls(&json!(calls));
                }
                json!({"model": self.model, "created_at": now(), "message": m, "done": true})
            }
            Kind::Generate => json!({"model": self.model, "created_at": now(), "response": "", "done": true}),
        };
        v["done_reason"] = json!(done_reason(self.finish.as_deref()));
        extend(
            &mut v,
            metrics(self.usage.as_ref(), self.timings.as_ref(), self.began.elapsed()),
        );
        out.push(v);
    }
}

/// `/api/embed` and `/api/embeddings` requests as an OpenAI embeddings body (`input`).
pub fn embed_body(req: &Value, model: &str) -> Value {
    let input = req
        .get("input")
        .or_else(|| req.get("prompt"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({"model": model, "input": input})
}

pub fn embed_answer(legacy: bool, model: &str, v: &Value, total: Duration) -> Value {
    let vectors: Vec<Value> = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| d["embedding"].clone())
        .collect();
    if legacy {
        return json!({"embedding": vectors.first().cloned().unwrap_or(json!([]))});
    }
    json!({"model": model, "embeddings": vectors, "total_duration": total.as_nanos() as u64, "load_duration": 0,
           "prompt_eval_count": v["usage"]["prompt_tokens"].as_u64().unwrap_or(0)})
}

fn file_time(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

/// A stable stand-in for Ollama's content digest (hashing multi-GB files on every listing would be slow): the id,
/// size and modification time.
fn digest(entry: &crate::catalog::ModelEntry) -> String {
    use sha2::{Digest, Sha256};
    let t = file_time(&entry.path);
    let h = Sha256::digest(format!("{}:{}:{t}", entry.id, entry.size_bytes).as_bytes());
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn details(entry: &crate::catalog::ModelEntry, parent: &str) -> Value {
    let s = entry.summary(false);
    let family = entry.architecture().unwrap_or("").to_string();
    json!({"parent_model": parent, "format": "gguf", "family": family, "families": [family],
           "parameter_size": s["size_label"].as_str().unwrap_or(""),
           "quantization_level": s["file_type"].as_str().unwrap_or("")})
}

/// One `/api/tags` row.
pub fn tag_entry(entry: &crate::catalog::ModelEntry, name: &str, parent: &str) -> Value {
    json!({"name": name, "model": name, "modified_at": file_time(&entry.path), "size": entry.size_bytes,
           "digest": digest(entry), "details": details(entry, parent)})
}

fn with_latest(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

/// `/api/tags`.
pub fn tags(hub: &Hub) -> Value {
    let e = &hub.engine;
    let cat = e.catalog();
    let mut models: Vec<Value> = cat.iter().map(|m| tag_entry(m, &m.id, "")).collect();
    for (name, p) in load_presets(&e.home) {
        if let Some(base) = cat.iter().find(|m| m.id == p.from) {
            models.push(tag_entry(base, &with_latest(&name), &p.from));
        }
    }
    json!({"models": models})
}

fn base_of(hub: &Hub, name: &str) -> Result<(crate::catalog::ModelEntry, Option<Preset>), Fail> {
    let preset = get_preset(&hub.engine.home, name);
    let target = preset.as_ref().map_or(name, |p| p.from.as_str());
    match hub.engine.resolve(target) {
        Ok(Target::Llama(e)) => Ok((e, preset)),
        Ok(Target::Ollama(_)) => Err(Fail::invalid("Ollama's own models are managed by Ollama")),
        Err(f) if f.status == 404 => Err(Fail::not_found(format!("model '{name}' not found"))),
        Err(f) => Err(f),
    }
}

/// `/api/show` (and the `model.show` operation).
pub fn show(hub: &Hub, name: &str) -> Result<Value, Fail> {
    let (entry, preset) = base_of(hub, name)?;
    let p = preset.clone().unwrap_or_else(|| Preset {
        from: entry.path.to_string_lossy().into_owned(),
        ..Default::default()
    });
    let mut params = String::new();
    for (k, v) in &p.parameters {
        for it in v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]) {
            params.push_str(&format!("{k} {}\n", plain(&it)));
        }
    }
    let template = p
        .template
        .clone()
        .or_else(|| {
            entry
                .meta
                .get("tokenizer.chat_template")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .unwrap_or_default();
    let mut caps = vec!["completion"];
    if entry.mmproj.is_some() {
        caps.push("vision");
    }
    let mut info = entry.meta.clone();
    info.remove("tokenizer.chat_template");
    Ok(
        json!({"license": p.license.clone().unwrap_or_default(), "modelfile": render_modelfile(&p),
              "parameters": params, "template": template, "system": p.system.clone().unwrap_or_default(),
              "details": details(&entry, preset.as_ref().map_or("", |p| p.from.as_str())),
              "model_info": info, "capabilities": caps, "modified_at": file_time(&entry.path)}),
    )
}

/// `/api/ps`.
pub fn ps(hub: &Hub) -> Value {
    let e = &hub.engine;
    let cat = e.catalog();
    let idle = e.settings().idle_unload_s;
    let models: Vec<Value> = e
        .loaded()
        .iter()
        .filter_map(|i| {
            let m = cat.iter().find(|m| m.id == i.model)?;
            let vram = i.serving().vram_mib.map_or(0, |mib| (mib * 1_048_576.0) as u64);
            // Ollama's "never expires" is the zero time
            let expires = match i.expires_in(idle) {
                Some(s) => (chrono::Utc::now() + chrono::Duration::seconds(s as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                None => "0001-01-01T00:00:00Z".into(),
            };
            let mut row = tag_entry(m, &m.id, "");
            row["size_vram"] = json!(vram);
            row["expires_at"] = json!(expires);
            row.as_object_mut()?.remove("modified_at");
            Some(row)
        })
        .collect();
    json!({"models": models})
}

fn taken(hub: &Hub, name: &str) -> Result<(), Fail> {
    if hub.engine.catalog().iter().any(|m| m.id == name) {
        return Err(Fail::new(
            409,
            "exists",
            format!("{name} is the name of a model file; pick another name"),
        ));
    }
    Ok(())
}

/// `/api/create` and the `model.create` operation: register a preset. Takes a Modelfile (`modelfile`) or its parts
/// (`from`, `system`, `template`, `parameters`, `adapters`, `messages`, `license`); parts win over the text.
pub fn create(hub: &Hub, a: &Value) -> Result<Value, Fail> {
    let name = a
        .get("model")
        .or_else(|| a.get("name"))
        .and_then(Value::as_str)
        .map(canon)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| Fail::invalid("model (the new name) is required"))?;
    let mut p = match a.get("modelfile").and_then(Value::as_str) {
        Some(text) => parse_modelfile(text).map_err(Fail::invalid)?,
        None => Preset::default(),
    };
    let text = |k: &str| a.get(k).and_then(Value::as_str).map(String::from);
    if let Some(f) = text("from") {
        p.from = f;
    }
    p.system = text("system").or(p.system);
    p.template = text("template").or(p.template);
    p.license = text("license").or(p.license);
    if let Some(o) = a.get("parameters").and_then(Value::as_object) {
        p.parameters.extend(o.clone());
    }
    match a.get("adapters") {
        // Ollama's {name: digest}
        Some(Value::Object(o)) => p.adapters.extend(o.keys().cloned()),
        Some(Value::Array(v)) => p.adapters.extend(v.iter().filter_map(Value::as_str).map(String::from)),
        _ => {}
    }
    if let Some(m) = a.get("messages").and_then(Value::as_array) {
        p.messages.extend(m.iter().cloned());
    }
    if p.from.is_empty() {
        return Err(Fail::invalid("a Modelfile needs a FROM line"));
    }
    taken(hub, &name)?;
    // FROM: an MM model (pulled from Hugging Face or the Ollama library, or found on disk), or another preset
    let (base, parent) = base_of(hub, &p.from).map_err(|f| {
        if f.status == 404 {
            Fail::not_found(format!(
                "FROM {}: no such model here; pull it first (model.pull)",
                p.from
            ))
        } else {
            f
        }
    })?;
    if let Some(par) = parent {
        // flatten: the new preset sits on the other's base and keeps its defaults unless it says otherwise
        let mut merged = par.parameters;
        merged.extend(std::mem::take(&mut p.parameters));
        p.parameters = merged;
        p.system = p.system.or(par.system);
        p.template = p.template.or(par.template);
        p.license = p.license.or(par.license);
        let mut ad = par.adapters;
        ad.extend(std::mem::take(&mut p.adapters));
        p.adapters = ad;
        let mut ms = par.messages;
        ms.extend(std::mem::take(&mut p.messages));
        p.messages = ms;
    }
    p.from = base.id.clone();
    let base_dir = a.get("base_dir").and_then(Value::as_str).map(std::path::PathBuf::from);
    let mut adapters: Vec<String> = vec![];
    for ad in &p.adapters {
        let rel = base_dir.as_ref().map(|d| d.join(ad)).filter(|j| j.is_file());
        let want = rel.map_or(ad.clone(), |j| j.to_string_lossy().into_owned());
        let found = hub::find_gguf(&hub.engine.home, "lora", &want).map_err(Fail::from)?;
        let found = found.to_string_lossy().into_owned();
        if !adapters.contains(&found) {
            adapters.push(found);
        }
    }
    p.adapters = adapters;
    p.created = now();
    let n = (p.parameters.len(), p.adapters.len(), p.messages.len());
    let from = p.from.clone();
    edit_presets(&hub.engine.home, |all| {
        all.insert(name.clone(), p);
        Ok(())
    })?;
    Ok(json!({"name": name, "from": from, "parameters": n.0, "adapters": n.1, "messages": n.2, "status": "success"}))
}

/// `/api/copy` and `model.copy`: a copy of a preset, or a new preset on a plain model.
pub fn copy(hub: &Hub, source: &str, destination: &str) -> Result<Value, Fail> {
    let dest = canon(destination);
    if dest.is_empty() {
        return Err(Fail::invalid("destination is required"));
    }
    taken(hub, &dest)?;
    let (base, preset) = base_of(hub, source)?;
    let mut p = preset.unwrap_or_else(|| Preset {
        from: base.id.clone(),
        ..Default::default()
    });
    p.created = now();
    edit_presets(&hub.engine.home, |all| {
        all.insert(dest.clone(), p);
        Ok(())
    })?;
    Ok(json!({"source": source, "destination": dest, "from": base.id}))
}

// ---- the routes -----------------------------------------------------------------------------------------------

/// A failed llama-server answer as the shim's error: its stable code when it has one (see `hub::upstream_error`), else
/// its own status and message as `backend_error`.
fn upstream_fail(status: u16, text: &str) -> Fail {
    hub::upstream_failure(status, text)
        .unwrap_or_else(|| Fail::new(status, "backend_error", hub::upstream_message(status, text)))
}

/// Ollama's errors are {"error": "message"}; the shim adds the hub's `code`, and the fields and `retry_after_s` of a
/// stable code.
pub struct OErr(Fail);

impl From<Fail> for OErr {
    fn from(f: Fail) -> Self {
        Self(f)
    }
}

impl From<ApiError> for OErr {
    fn from(e: ApiError) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for OErr {
    fn into_response(self) -> Response {
        let f = self.0;
        let status = StatusCode::from_u16(f.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut body = json!({"error": f.message, "code": f.code});
        f.add_fields(&mut body);
        let mut resp = (status, Json(body)).into_response();
        if let Some(wait) = f.retry_after {
            resp.headers_mut()
                .insert("retry-after", wait.to_string().parse().unwrap());
        }
        resp
    }
}

type R<T> = Result<T, OErr>;

pub fn routes() -> Router<Hub> {
    Router::new()
        .route("/api/version", get(version))
        .route("/api/tags", get(tags_route))
        .route("/api/ps", get(ps_route))
        .route("/api/show", post(show_route))
        .route("/api/chat", post(chat_route))
        .route("/api/generate", post(generate_route))
        .route("/api/embed", post(embed_route))
        .route("/api/embeddings", post(embeddings_route))
        .route("/api/pull", post(pull_route))
        .route("/api/create", post(create_route))
        .route("/api/copy", post(copy_route))
        .route("/api/delete", delete(delete_route).post(delete_route))
}

async fn version() -> Json<Value> {
    Json(json!({"version": hub::VERSION}))
}

async fn tags_route(State(hub): State<Hub>) -> Json<Value> {
    Json(tags(&hub))
}

async fn ps_route(State(hub): State<Hub>) -> Json<Value> {
    Json(ps(&hub))
}

fn model_of(req: &Value) -> R<String> {
    req.get("model")
        .or_else(|| req.get("name"))
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Fail::invalid("model is required").into())
}

async fn show_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Json<Value>> {
    Ok(Json(show(&hub, &model_of(&req)?)?))
}

/// The model a request means (a preset's base), with the preset's adapters attached to it.
async fn prepare(hub: &Hub, asked: &str) -> R<(String, Option<Preset>)> {
    let Some(p) = get_preset(&hub.engine.home, asked) else {
        return Ok((asked.to_string(), None));
    };
    for ad in &p.adapters {
        let attached = hub
            .engine
            .extras(&p.from)
            .lora
            .iter()
            .any(|l| l.path.as_path() == Path::new(ad));
        if !attached {
            let args = json!({"model": p.from, "action": "attach", "adapter": ad});
            hub::call(hub, "lora", &args).await?;
        }
    }
    Ok((p.from.clone(), Some(p)))
}

type Lines = futures::stream::BoxStream<'static, Result<String, std::io::Error>>;

fn ndjson(lines: Lines) -> Response {
    (
        [("content-type", "application/x-ndjson")],
        axum::body::Body::from_stream(lines),
    )
        .into_response()
}

fn line(v: &Value) -> String {
    format!("{v}\n")
}

/// Send `body` to llama-server at `path` and answer in Ollama's shape, streamed or not.
async fn run(hub: Hub, req: &Value, path: &'static str, body: Value, kind: Kind, shown: String) -> R<Response> {
    let stream = streaming(req);
    let keep = req.get("keep_alive").and_then(keep_alive_s);
    let t0 = Instant::now();
    let (model, resp, lease) = hub.engine.forward(path, body).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(upstream_fail(status.as_u16(), &text).into());
    }
    if !stream {
        let v: Value = resp.json().await.map_err(|e| Fail::unavailable(e.to_string()))?;
        hub.engine
            .record(&model, true, t0.elapsed().as_millis() as u64, v.get("usage"));
        drop(lease);
        hub.engine.set_keep_alive(&model, keep);
        return Ok(Json(answer(kind, &shown, &v, t0.elapsed())).into_response());
    }
    hub.engine.record(&model, true, t0.elapsed().as_millis() as u64, None);
    let mut conv = Ndjson::new(kind, &shown);
    let mut upstream = resp.bytes_stream();
    Ok(ndjson(
        async_stream::stream! {
            while let Some(chunk) = upstream.next().await {
                match chunk {
                    Ok(b) => {
                        for v in conv.push(&b) {
                            yield Ok(line(&v));
                        }
                    }
                    Err(e) => {
                        yield Ok(line(&json!({"error": e.to_string()})));
                        break;
                    }
                }
            }
            for v in conv.finish() {
                yield Ok(line(&v));
            }
            drop(lease);
            hub.engine.set_keep_alive(&model, keep);
        }
        .boxed(),
    ))
}

/// An empty request loads the model (or, with `keep_alive: 0`, unloads it): Ollama's idiom.
async fn load_only(hub: &Hub, req: &Value, real: &str, shown: &str, kind: Kind) -> R<Response> {
    let keep = req.get("keep_alive").and_then(keep_alive_s);
    let reason = if keep == Some(0) {
        hub.engine.unload(real);
        "unload"
    } else {
        hub::call(hub, "model.load", &json!({"model": real})).await?;
        hub.engine.set_keep_alive(real, keep);
        "load"
    };
    let mut v = json!({"model": shown, "created_at": now(), "done": true, "done_reason": reason});
    match kind {
        Kind::Chat => v["message"] = json!({"role": "assistant", "content": ""}),
        Kind::Generate => v["response"] = json!(""),
    }
    Ok(Json(v).into_response())
}

async fn chat_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Response> {
    let asked = model_of(&req)?;
    let (real, preset) = prepare(&hub, &asked).await?;
    if req["messages"].as_array().is_none_or(|m| m.is_empty()) {
        return load_only(&hub, &req, &real, &asked, Kind::Chat).await;
    }
    let body = chat_body(&req, &real, preset.as_ref());
    run(hub, &req, "/v1/chat/completions", body, Kind::Chat, asked).await
}

async fn generate_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Response> {
    let asked = model_of(&req)?;
    let (real, preset) = prepare(&hub, &asked).await?;
    let Some(prompt) = req["prompt"].as_str().filter(|p| !p.is_empty()) else {
        return load_only(&hub, &req, &real, &asked, Kind::Generate).await;
    };
    let text = |k: &str| req.get(k).and_then(Value::as_str);
    let system = text("system").or(preset.as_ref().and_then(|p| p.system.as_deref()));
    let template = text("template").or(preset.as_ref().and_then(|p| p.template.as_deref()));
    let raw = req["raw"].as_bool().unwrap_or(false);
    if raw || template.is_some() {
        // the completions path: the prompt as it is, or through the template
        let full = match (raw, template) {
            (false, Some(t)) => render_template(t, system.unwrap_or(""), prompt),
            _ => prompt.to_string(),
        };
        let mut body = json!({"model": real, "prompt": full});
        finish_body(&mut body, &req, preset.as_ref());
        return run(hub, &req, "/v1/completions", body, Kind::Generate, asked).await;
    }
    // otherwise the model's own chat template applies, as in Ollama
    let mut messages = vec![];
    if let Some(s) = system {
        messages.push(json!({"role": "system", "content": s}));
    }
    let user = json!({"role": "user", "content": prompt, "images": req.get("images").cloned().unwrap_or(Value::Null)});
    messages.push(openai_message(&user));
    let mut body = json!({"model": real, "messages": messages});
    finish_body(&mut body, &req, preset.as_ref());
    run(hub, &req, "/v1/chat/completions", body, Kind::Generate, asked).await
}

async fn embeddings_call(hub: &Hub, req: &Value, legacy: bool) -> R<Json<Value>> {
    let asked = model_of(req)?;
    let (real, _) = prepare(hub, &asked).await?;
    let body = embed_body(req, &real);
    crate::tools::check_embeddings(&body).map_err(Fail::invalid)?;
    let t0 = Instant::now();
    let (model, resp, lease) = hub.engine.forward("/v1/embeddings", body).await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    drop(lease);
    hub.engine
        .set_keep_alive(&model, req.get("keep_alive").and_then(keep_alive_s));
    hub.engine.record(
        &model,
        status.is_success(),
        t0.elapsed().as_millis() as u64,
        v.get("usage"),
    );
    if !status.is_success() {
        return Err(upstream_fail(status.as_u16(), &text).into());
    }
    Ok(Json(embed_answer(legacy, &asked, &v, t0.elapsed())))
}

async fn embed_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Json<Value>> {
    embeddings_call(&hub, &req, false).await
}

async fn embeddings_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Json<Value>> {
    embeddings_call(&hub, &req, true).await
}

async fn pull_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Response> {
    let name = model_of(&req)?;
    let job = hub::call(&hub, "model.pull", &json!({"model": name})).await?;
    let id = job["job"].as_str().unwrap_or("").to_string();
    if !streaming(&req) {
        let j = loop {
            let Some(j) = hub.jobs.get(&id) else {
                break json!({"status": "failed", "message": "job lost"});
            };
            let d = j.describe();
            if d["status"] != "running" {
                break d;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        return match j["status"].as_str() {
            Some("done") => Ok(Json(json!({"status": "success"})).into_response()),
            _ => Err(Fail::new(
                500,
                "pull_failed",
                j["message"].as_str().unwrap_or("the download failed"),
            )
            .into()),
        };
    }
    Ok(ndjson(
        async_stream::stream! {
            yield Ok(line(&json!({"status": "pulling manifest"})));
            let mut last = String::new();
            while let Some(j) = hub.jobs.get(&id) {
                let d = j.describe();
                if d["status"] != "running" {
                    if d["status"] == "done" {
                        yield Ok(line(&json!({"status": "success"})));
                    } else {
                        yield Ok(line(&json!({"error": d["message"]})));
                    }
                    break;
                }
                let (done, total) = (d["done_bytes"].as_u64().unwrap_or(0), d["total_bytes"].as_u64().unwrap_or(0));
                let status = format!("pulling {}", d["model"].as_str().unwrap_or(""));
                let key = format!("{status}{done}");
                if key != last {
                    last = key;
                    yield Ok(line(&json!({"status": status, "total": total, "completed": done})));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        .boxed(),
    ))
}

async fn create_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Response> {
    let done = create(&hub, &req)?;
    if streaming(&req) {
        let rows = ["using existing layer", "writing manifest", "success"]
            .map(|s| Ok::<_, std::io::Error>(line(&json!({"status": s}))));
        return Ok(ndjson(futures::stream::iter(rows).boxed()));
    }
    Ok(Json(json!({"status": "success", "name": done["name"]})).into_response())
}

async fn copy_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Json<Value>> {
    let src = req["source"]
        .as_str()
        .ok_or_else(|| Fail::invalid("source is required"))?;
    let dst = req["destination"]
        .as_str()
        .ok_or_else(|| Fail::invalid("destination is required"))?;
    copy(&hub, src, dst)?;
    Ok(Json(json!({})))
}

async fn delete_route(State(hub): State<Hub>, Json(req): Json<Value>) -> R<Json<Value>> {
    let name = model_of(&req)?;
    let removed = edit_presets(&hub.engine.home, |all| Ok(all.remove(&canon(&name)).is_some()))?;
    if !removed {
        // not a preset: a model MM downloaded (model.delete refuses anything else)
        hub::call(&hub, "model.delete", &json!({"model": name})).await?;
    }
    Ok(Json(json!({})))
}

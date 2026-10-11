//! Calling a specialist KM: `km.specialist.call {id, input}`.
//!
//! A specialist KM binds a model the hub serves (`specialist.model`) to an engine (`stt`, `tts`, `image`, `vision`)
//! and may carry an `input_schema` (JSON Schema, as an inline table or as a JSON string). A call needs the KM to be
//! loaded; the input is checked against the schema, handed to the hub operation of the engine with the KM's model
//! filled in, and recorded as a specialist call in the trace store (km-runtime.md).

use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::manifest::Manifest;
use super::{Km, Res};
use crate::engine::Fail;
use crate::hub::Hub;

/// What runs a specialist's engine. The real one calls the hub's own operations; tests put a stub in its place.
pub trait Backend: Send + Sync {
    /// `engine` is `stt`, `tts`, `image` or `vision`; `args` already hold the KM's model.
    fn run<'a>(&'a self, hub: &'a Hub, engine: &'a str, args: Value) -> BoxFuture<'a, Res<Value>>;
}

/// The hub operation behind each engine.
pub fn hub_op(engine: &str) -> Option<&'static str> {
    Some(match engine {
        "stt" => "speech.transcribe",
        "tts" => "speech.synthesize",
        "image" => "image.generate",
        "vision" => "vision.analyze",
        _ => return None,
    })
}

pub struct HubBackend;

impl Backend for HubBackend {
    fn run<'a>(&'a self, hub: &'a Hub, engine: &'a str, mut args: Value) -> BoxFuture<'a, Res<Value>> {
        Box::pin(async move {
            let op = hub_op(engine).ok_or_else(|| Fail::invalid(format!("no engine {engine:?}")))?;
            if engine == "image" && args.get("wait").is_none() {
                args["wait"] = json!(true);
            }
            crate::hub::call(hub, op, &args).await.map_err(|e| {
                let f = Fail::from(e);
                // an engine that is not built in, or a model it does not have, is the engine being unavailable
                if matches!(f.status, 404 | 501) {
                    Fail::unavailable(format!("{engine} engine: {}", f.message))
                } else {
                    f
                }
            })
        })
    }
}

/// The schema a manifest gives for its input, if any.
fn input_schema(m: &Manifest) -> Res<Option<Value>> {
    let Some(v) = m.raw.get("specialist").and_then(|s| s.get("input_schema")) else {
        return Ok(None);
    };
    match v {
        toml::Value::String(s) => serde_json::from_str(s)
            .map(Some)
            .map_err(|e| Fail::invalid(format!("specialist.input_schema is not JSON: {e}"))),
        other => Ok(serde_json::to_value(other).ok()),
    }
}

/// Long strings (a picture as base64) are not worth keeping in a trace.
fn shortened(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > 2000 => json!(format!("<{} characters>", s.chars().count())),
        Value::Array(a) => Value::Array(a.iter().map(shortened).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), shortened(x))).collect()),
        other => other.clone(),
    }
}

impl Km {
    pub fn specialist_backend(&self) -> Arc<dyn Backend> {
        self.backend.lock().clone().unwrap_or_else(|| Arc::new(HubBackend))
    }

    pub fn set_specialist_backend(&self, b: Option<Arc<dyn Backend>>) {
        *self.backend.lock() = b;
    }

    /// Calls a loaded specialist KM. `trace` files the call under a trace that already exists; otherwise the call
    /// gets a trace of its own (kind `km.specialist`).
    pub async fn specialist_call(
        &self,
        hub: &Hub,
        id: &str,
        version: Option<&str>,
        channel: Option<&str>,
        input: &Value,
        trace: Option<&str>,
    ) -> Res<Value> {
        let rec = self.store.resolve(id, version, channel)?;
        if rec.kind != "specialist" {
            return Err(Fail::invalid(format!(
                "{} is a {} KM, not a specialist",
                rec.key, rec.kind
            )));
        }
        if !self.manager.is_resident(&rec.key) {
            return Err(Fail::not_found(format!("{} is not loaded (km.load loads it)", rec.key)));
        }
        let m = Manifest::from_file(&self.store.dir_of(&rec.key).join("km.toml"))?;
        let spec = m
            .specialist
            .clone()
            .ok_or_else(|| Fail::invalid(format!("{} has no [specialist] table", rec.key)))?;
        if !input.is_object() {
            return Err(Fail::invalid("input must be a JSON object"));
        }
        if let Some(schema) = input_schema(&m)? {
            let problems = crate::factory::checks::validate(&schema, input);
            if !problems.is_empty() {
                return Err(Fail::invalid(format!("input: {}", problems.join("; "))));
            }
        }
        let mut args = input.clone();
        args["model"] = json!(spec.model);
        let tr = crate::trace::tracer(&self.engine.home);
        let (trace_id, own) = match trace {
            Some(t) => (Some(t.to_string()), false),
            None => (
                tr.start(
                    "km.specialist",
                    &spec.model,
                    &json!({"km": rec.key, "engine": spec.engine, "input": shortened(input)}),
                    None,
                    &json!({"km": rec.key}),
                ),
                true,
            ),
        };
        let _hold = self.manager.guard(std::slice::from_ref(&rec.key));
        self.manager.touch(&rec.key);
        let t0 = Instant::now();
        let ran = self.specialist_backend().run(hub, &spec.engine, args).await;
        let ms = t0.elapsed().as_millis() as u64;
        if let Some(t) = &trace_id {
            let out = match &ran {
                Ok(v) => shortened(v),
                Err(e) => json!({"error": e.message, "code": e.code}),
            };
            tr.specialist_call(t, &rec.key, shortened(input), out, ms, ran.is_ok());
            if own {
                let err = ran.as_ref().err().map(|e| e.message.as_str());
                tr.finish(t, if ran.is_ok() { "ok" } else { "error" }, err);
            }
        }
        let result = ran?;
        Ok(
            json!({"km": rec.key, "engine": spec.engine, "model": spec.model, "ms": ms, "trace": trace_id,
                  "result": result}),
        )
    }
}

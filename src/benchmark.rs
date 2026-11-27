//! The device benchmark: how fast this machine is, how fast each installed chat model runs on it, and which of them
//! suit it. Three measures, as on Android: a CPU stress (does the speed hold, or does the machine slow down under
//! load?), a memory copy (the bandwidth that decides how fast a model's weights can be read), and one short reply from
//! each model. The suggestions are worked out from those, in `suggest`, which is pure and tested.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sysinfo::System;

use crate::engine::{Fail, Result};
use crate::hub::Hub;

/// Reading speed in tokens per second: a model at or above it keeps up with a person reading the reply.
pub const READING_TOKENS_PER_S: f64 = 8.0;

pub fn operations() -> Vec<Value> {
    vec![json!({
        "id": "system.bench",
        "group": "system",
        "summary": "Benchmark this machine: a CPU stress, a memory copy, and one short reply from each installed chat model; then suggest the models that suit it. Takes about a minute per model on a CPU",
        "mutating": true,
        "destructive": false,
        "input_schema": {"type": "object", "properties": {
            "seconds": {"type": "number", "minimum": 3, "maximum": 60, "description": "how long the CPU stress runs (default 10)"},
            "tokens": {"type": "integer", "minimum": 16, "maximum": 512, "description": "the length of each model's reply (default 96)"},
            "models": {"type": "array", "items": {"type": "string"}, "description": "the models to time (default: every installed chat model)"}
        }, "required": [], "additionalProperties": false}
    })]
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Result<Value> {
    match id {
        "system.bench" => run(hub, a).await,
        other => Err(Fail::new(404, "not_found", format!("no operation {other}"))),
    }
}

/// Models that are not chat models (their names say so): embedders, rerankers, speech and image models.
fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "rerank", "whisper", "tts", "clip", "stable-diffusion"].iter().any(|w| id.contains(w))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

async fn run(hub: &Hub, a: &Value) -> Result<Value> {
    let seconds = a["seconds"].as_f64().unwrap_or(10.0).clamp(3.0, 60.0);
    let tokens = a["tokens"].as_u64().unwrap_or(96).clamp(16, 512) as u32;
    let wanted: Option<Vec<String>> = a["models"]
        .as_array()
        .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    let cpu = tokio::task::spawn_blocking(move || cpu_stress(seconds))
        .await
        .map_err(|e| Fail::new(500, "internal", e.to_string()))?;
    let memory = tokio::task::spawn_blocking(|| memory_copy_gbps(2.0))
        .await
        .map_err(|e| Fail::new(500, "internal", e.to_string()))?;
    let mut sys = System::new();
    sys.refresh_memory();
    let ram_gb = sys.total_memory() as f64 / 1e9;

    let mut models: Vec<crate::catalog::ModelEntry> = hub
        .engine
        .catalog()
        .into_iter()
        .filter(|m| is_chat_model(&m.id))
        .filter(|m| wanted.as_ref().is_none_or(|w| w.contains(&m.id)))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));

    let mut rows = Vec::new();
    for m in &models {
        let cold = !hub.engine.is_loaded(&m.id);
        let size_gb = round1(m.size_bytes as f64 / 1e9);
        let body = json!({
            "model": m.id,
            "messages": [{"role": "user", "content": "Write two sentences about the sea."}],
            "max_tokens": tokens,
            "temperature": 0.7,
            "stream": false,
            "chat_template_kwargs": {"enable_thinking": false},
        });
        let t0 = Instant::now();
        // the lease keeps the model in memory until the reply is read
        let timed = match hub.engine.forward("/v1/chat/completions", body).await {
            Ok((_, resp, _lease)) => match resp.json::<Value>().await {
                Ok(v) => Ok(v["usage"]["completion_tokens"].as_u64().unwrap_or(0)),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.message),
        };
        let ms = t0.elapsed().as_millis() as u64;
        rows.push(match timed {
            Ok(n) => json!({
                "model": m.id,
                "size_gb": size_gb,
                "tokens": n,
                "ms": ms,
                "tokens_per_s": if ms > 0 { json!(round1(n as f64 * 1000.0 / ms as f64)) } else { Value::Null },
                "cold_start": cold,
            }),
            Err(e) => json!({"model": m.id, "size_gb": size_gb, "error": e}),
        });
    }

    let candidates: Vec<Candidate> = rows
        .iter()
        .filter_map(|r| {
            Some(Candidate {
                id: r["model"].as_str()?.to_string(),
                size_gb: r["size_gb"].as_f64()?,
                // a cold start includes loading the model, so only a warm run says how fast it is
                tok_s: r["tokens_per_s"].as_f64().filter(|_| !r["cold_start"].as_bool().unwrap_or(false)),
            })
        })
        .collect();
    let suggestions = suggest(&candidates, Some(ram_gb), Some(memory));
    Ok(json!({
        "cpu": cpu,
        "memory_gbps": memory,
        "ram_gb": round1(ram_gb),
        "models": rows,
        "suggestions": suggestions,
        "note": "A cold start includes loading the model, so its speed is left out of the suggestions; run the benchmark again to time it warm.",
    }))
}

/// A CPU stress on every logical core for `seconds`. The work is counted in half-second buckets, so a machine that
/// slows down under sustained load shows it: `steady` is the speed at the end over the speed at the start (1.0 means
/// it held).
pub fn cpu_stress(seconds: f64) -> Value {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let buckets = ((seconds * 2.0).ceil() as usize).max(1);
    let counts: Arc<Vec<AtomicU64>> = Arc::new((0..buckets).map(|_| AtomicU64::new(0)).collect());
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(seconds);
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let counts = counts.clone();
            std::thread::spawn(move || {
                let mut x: f64 = 1.0;
                while Instant::now() < deadline {
                    let b = (((Instant::now() - start).as_secs_f64() * 2.0) as usize).min(buckets - 1);
                    for _ in 0..20_000 {
                        x = (x * 1.000_000_1 + 0.5).sin().abs() + 1e-9;
                    }
                    std::hint::black_box(x);
                    counts[b].fetch_add(20_000, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    // work per second, from the half-second buckets
    let per_s: Vec<f64> = counts.iter().map(|c| c.load(Ordering::Relaxed) as f64 * 2.0).collect();
    let fifth = (per_s.len() / 5).max(1);
    let first = per_s[..fifth].iter().sum::<f64>() / fifth as f64;
    let last = per_s[per_s.len() - fifth..].iter().sum::<f64>() / fifth as f64;
    let steady = if first > 0.0 { last / first } else { 0.0 };
    json!({
        "threads": threads,
        "seconds": seconds,
        "first_speed": round1(first),
        "last_speed": round1(last),
        "steady": (steady * 100.0).round() / 100.0,
    })
}

/// The bandwidth of a copy between two 64 MB buffers, in GB/s, measured for about `seconds`.
pub fn memory_copy_gbps(seconds: f64) -> f64 {
    const SIZE: usize = 64 << 20;
    let src = vec![1u8; SIZE];
    let mut dst = vec![0u8; SIZE];
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs_f64(seconds);
    let mut copied: u64 = 0;
    while Instant::now() < deadline {
        dst.copy_from_slice(&src);
        std::hint::black_box(&dst);
        copied += SIZE as u64;
    }
    let secs = t0.elapsed().as_secs_f64();
    if secs > 0.0 {
        round1(copied as f64 / secs / 1e9)
    } else {
        0.0
    }
}

/// A model the suggestions are worked out for: its size, and its measured speed if it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub size_gb: f64,
    pub tok_s: Option<f64>,
}

/// The models that suit this machine. A model's speed is the one measured, or else estimated from its size: decoding
/// reads the weights once per token, so speed is about the memory bandwidth divided by the size. The bandwidth is the
/// median of the measured runs (tokens per second times size), and the copy benchmark stands in when nothing was
/// measured. A model fits when its size is at most 80% of the RAM.
///
/// - `fastest`: the fitting model with the highest speed.
/// - `balanced`: the largest fitting model that keeps up with reading (`READING_TOKENS_PER_S`); the fastest when none does.
/// - `best`: the largest fitting model, for when quality matters more than speed.
pub fn suggest(models: &[Candidate], ram_gb: Option<f64>, copy_gbps: Option<f64>) -> Value {
    let mut implied: Vec<f64> = models
        .iter()
        .filter_map(|m| m.tok_s.map(|t| t * m.size_gb))
        .filter(|v| *v > 0.0)
        .collect();
    implied.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let bandwidth = if implied.is_empty() {
        copy_gbps.filter(|v| *v > 0.0)
    } else {
        Some(implied[implied.len() / 2])
    };
    let speed = |m: &Candidate| -> Option<f64> {
        m.tok_s
            .or_else(|| bandwidth.filter(|_| m.size_gb > 0.0).map(|bw| bw / m.size_gb))
    };
    let fits = |m: &Candidate| ram_gb.is_none_or(|r| m.size_gb <= 0.8 * r);
    let usable: Vec<(&Candidate, f64)> = models
        .iter()
        .filter(|m| fits(m))
        .filter_map(|m| speed(m).map(|s| (m, s)))
        .collect();
    let fastest = usable
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(m, _)| *m);
    let balanced = usable
        .iter()
        .filter(|(_, s)| *s >= READING_TOKENS_PER_S)
        .max_by(|a, b| a.0.size_gb.partial_cmp(&b.0.size_gb).unwrap())
        .map(|(m, _)| *m)
        .or(fastest);
    let best = usable
        .iter()
        .max_by(|a, b| a.0.size_gb.partial_cmp(&b.0.size_gb).unwrap())
        .map(|(m, _)| *m);
    let name = |m: Option<&Candidate>| m.map(|c| json!(c.id)).unwrap_or(Value::Null);
    json!({
        "fastest": name(fastest),
        "balanced": name(balanced),
        "best": name(best),
        "bandwidth_gbps": bandwidth.map(round1),
        "reading_tokens_per_s": READING_TOKENS_PER_S,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, size_gb: f64, tok_s: Option<f64>) -> Candidate {
        Candidate { id: id.into(), size_gb, tok_s }
    }

    #[test]
    fn the_suggestions_follow_the_measured_speeds_and_the_ram() {
        let models = [
            c("tiny", 1.0, Some(30.0)),
            c("mid", 4.0, Some(9.0)),
            c("big", 8.0, Some(3.0)),
            c("huge", 20.0, Some(1.0)),
        ];
        let s = suggest(&models, Some(16.0), None);
        assert_eq!(s["fastest"], "tiny");
        assert_eq!(s["balanced"], "mid", "the largest that keeps up with reading, and fits");
        assert_eq!(s["best"], "big", "the largest that fits: huge needs more than 80% of 16 GB");
    }

    #[test]
    fn an_unmeasured_model_is_estimated_from_the_bandwidth_the_runs_imply() {
        // 10 tokens/s of a 4 GB model is 40 GB/s; a 2 GB model then reads at 20 tokens/s
        let models = [c("measured", 4.0, Some(10.0)), c("guess", 2.0, None)];
        let s = suggest(&models, Some(32.0), None);
        assert_eq!(s["bandwidth_gbps"], 40.0);
        assert_eq!(s["fastest"], "guess");
        // with nothing measured, the copy benchmark is the bandwidth
        let none = suggest(&[c("only", 2.0, None)], Some(32.0), Some(12.0));
        assert_eq!(none["fastest"], "only");
        assert_eq!(none["bandwidth_gbps"], 12.0);
    }

    #[test]
    fn nothing_is_suggested_when_no_speed_is_known_or_nothing_fits() {
        let s = suggest(&[c("a", 1.0, None)], None, None);
        assert_eq!(s["fastest"], Value::Null, "no speed is known for it");
        let s = suggest(&[c("big", 30.0, Some(5.0))], Some(16.0), None);
        assert_eq!(s["best"], Value::Null, "it does not fit in 16 GB");
        assert_eq!(s["balanced"], Value::Null);
    }

    #[test]
    fn the_cpu_stress_reports_its_threads_and_how_it_held() {
        let r = cpu_stress(3.0);
        assert!(r["threads"].as_u64().unwrap() >= 1);
        assert!(r["first_speed"].as_f64().unwrap() > 0.0);
        assert!(r["steady"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn a_copy_moves_memory_at_a_positive_rate() {
        assert!(memory_copy_gbps(0.2) > 0.0);
    }

    #[test]
    fn embedders_and_speech_models_are_not_timed_as_chat_models() {
        assert!(is_chat_model("LFM2.5-1.2B-Instruct-Q4_K_M"));
        assert!(!is_chat_model("nomic-embed-text"));
        assert!(!is_chat_model("whisper-base"));
    }
}

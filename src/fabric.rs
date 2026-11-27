//! ComputeFabric from this hub's side.
//!
//! ComputeFabric (CF) is a fabric of paired nodes. Each node can run a ModelMistress hub, and CF routes model requests
//! between them (see `docs/fabric.md`, and CF's `docs/MODELMISTRESS-INTEGRATION.md`). This module is the hub's half:
//!
//! - `machine.status`: the room a load has here (free VRAM and RAM, the commit), which CF reads to decide where a
//!   model fits.
//! - an **offload**: a request this hub cannot serve for lack of memory (or because it is busy) is passed to CF's
//!   endpoint instead of failing, when `<home>/fabric.json` says so. A request that already came through the fabric
//!   (it carries `x-cf-hop`) is never passed on again.
//! - `fabric.*` operations: the nodes, where a model would run, and the settings.
//!
//! Nothing here runs unless `<home>/fabric.json` exists. The hub does not need CF.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engine::Fail;
use crate::hub::Hub;

const FILE: &str = "fabric.json";
const HOP_HEADER: &str = "x-cf-hop";

type Res<T> = Result<T, Fail>;

/// Requests this process passed to the fabric, by hub home and the node that served them. A hub does not read the
/// answer's token counts into its own model usage; the caller's key gets them (see `Offloaded::into_response`).
static OFFLOADED: Mutex<BTreeMap<(PathBuf, String), u64>> = Mutex::new(BTreeMap::new());

fn count_offloaded(home: &Path, node: &str) {
    if let Ok(mut m) = OFFLOADED.lock() {
        *m.entry((home.to_path_buf(), node.to_string())).or_insert(0) += 1;
    }
}

fn offloaded_by_node(home: &Path) -> Value {
    let m = OFFLOADED.lock().map(|m| m.clone()).unwrap_or_default();
    json!(m
        .into_iter()
        .filter(|((h, _), _)| h == home)
        .map(|((_, node), n)| (node, n))
        .collect::<BTreeMap<_, _>>())
}

/// A request the fabric served for this hub: its answer, not yet sent on.
pub struct Offloaded {
    resp: reqwest::Response,
    /// The node that served it (`x-cf-node` from the fabric endpoint).
    pub node: Option<String>,
}

impl Offloaded {
    /// The answer to send to the caller. `observe` sees every piece of it as it passes, so the caller's key can be
    /// charged for the tokens, as for a request served here.
    pub fn into_response(self, mut observe: impl FnMut(&[u8]) + Send + 'static) -> Response {
        let content_type = self.resp.headers().get("content-type").cloned();
        let mut out = Response::new(Body::from_stream(self.resp.bytes_stream().map(move |c| {
            if let Ok(b) = &c {
                observe(b);
            }
            c.map_err(std::io::Error::other)
        })));
        *out.status_mut() = StatusCode::OK;
        if let Some(c) = content_type {
            out.headers_mut().insert("content-type", c);
        }
        if let Some(n) = self.node.as_deref().and_then(|n| HeaderValue::from_str(n).ok()) {
            out.headers_mut().insert("x-mm-node", n);
        }
        out.headers_mut().insert("x-mm-offloaded", HeaderValue::from_static("1"));
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offload {
    /// Never pass a request to the fabric.
    Off,
    /// Pass a request the hub cannot serve for lack of memory, or because it is busy.
    #[default]
    WhenFull,
}

/// `<home>/fabric.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FabricConfig {
    /// CF's API, for example `http://127.0.0.1:7878`.
    pub api: String,
    /// CF's OpenAI-compatible endpoint, for example `http://127.0.0.1:7880`.
    pub endpoint: String,
    /// A file holding the endpoint's bearer token (`cf models token`).
    pub token_file: Option<PathBuf>,
    /// A file holding a CF API token, when CF asks for one on loopback (`security.api_require_token`).
    pub api_token_file: Option<PathBuf>,
    pub offload: Offload,
}

/// The settings, or None when the hub is not joined to a fabric.
pub fn load(home: &Path) -> Option<FabricConfig> {
    let text = std::fs::read_to_string(home.join(FILE)).ok()?;
    let c: FabricConfig = serde_json::from_str(&text).ok()?;
    (!c.endpoint.is_empty() || !c.api.is_empty()).then_some(c)
}

fn save(home: &Path, c: &FabricConfig) -> Res<()> {
    let tmp = home.join(format!("{FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| Fail::new(500, "io", e.to_string()))?;
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, home.join(FILE)))
        .map_err(|e| Fail::new(500, "io", e.to_string()))
}

fn read_token(f: &Option<PathBuf>) -> Option<String> {
    let t = std::fs::read_to_string(f.as_ref()?).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

// ---- how far a request has travelled ------------------------------------------------------------------------

tokio::task_local! {
    static HOP: u32;
}

/// Middleware: remember the request's `x-cf-hop` for the rest of its handling.
pub async fn hop_layer(req: Request, next: Next) -> Response {
    let hop = req
        .headers()
        .get(HOP_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    HOP.scope(hop, next.run(req)).await
}

/// How many routers the request being handled has passed (0 for a request that came straight from a client).
pub fn hop() -> u32 {
    HOP.try_with(|h| *h).unwrap_or(0)
}

// ---- offload -----------------------------------------------------------------------------------------------

/// Whether a failure is one the fabric could cure: this hub lacks the memory, or is busy.
pub fn capacity_failure(f: &Fail) -> bool {
    matches!(f.code, "insufficient_memory" | "insufficient_vram" | "busy")
}

/// Whether a request that fails here should be kept for the fabric (so its body is copied before it is used).
pub fn offers(hub: &Hub) -> bool {
    hop() == 0
        && load(&hub.engine.home)
            .map(|c| c.offload == Offload::WhenFull && !c.endpoint.is_empty())
            .unwrap_or(false)
}

/// Pass a request to the fabric's endpoint. None when it cannot be (no fabric, offload off, the request came through the
/// fabric already, no node can serve it either): the caller then reports its own failure.
pub async fn offload(hub: &Hub, path: &str, body: &Value) -> Option<Offloaded> {
    if hop() > 0 {
        return None;
    }
    let cfg = load(&hub.engine.home)?;
    if cfg.offload == Offload::Off || cfg.endpoint.is_empty() {
        return None;
    }
    let mut req = hub
        .engine
        .http
        .post(format!("{}{path}", cfg.endpoint.trim_end_matches('/')))
        .header(HOP_HEADER, "1")
        .header("x-cf-caller", "model-mistress")
        .json(body);
    if let Some(t) = read_token(&cfg.token_file) {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        // The fabric could not serve it either: the hub's own error is the better answer.
        return None;
    }
    let node = resp
        .headers()
        .get("x-cf-node")
        .and_then(|n| n.to_str().ok())
        .map(String::from);
    count_offloaded(&hub.engine.home, node.as_deref().unwrap_or("?"));
    Some(Offloaded { resp, node })
}

// ---- operations ----------------------------------------------------------------------------------------------

fn op(id: &str, summary: &str, mutating: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": id.split('.').next().unwrap_or(id), "summary": summary, "mutating": mutating,
           "destructive": false,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    vec![
        op("machine.status", "The room a load has here: each GPU's free memory, what models may use of it, free RAM, the Windows commit, and what is running", false, json!({}), &[]),
        op("fabric.status", "Whether this hub is joined to a ComputeFabric, what it offloads, and whether the fabric answers", false, json!({}), &[]),
        op("fabric.nodes", "Every fabric node with a model hub: what each has loaded, its free VRAM and RAM, and how busy it is", false, json!({}), &[]),
        op("fabric.models", "Every model in the fabric and where it is installed and loaded", false, json!({}), &[]),
        op(
            "fabric.route",
            "Which fabric node would serve a request for a model now, and why",
            false,
            json!({"model": {"type": "string", "description": "a model id"}}),
            &["model"],
        ),
        op(
            "fabric.set",
            "Join this hub to a ComputeFabric, or change how: the CF API and endpoint addresses, the token files, and whether to offload (when_full or off). `leave` removes the settings",
            true,
            json!({
                "api": {"type": "string", "description": "CF's API, e.g. http://127.0.0.1:7878"},
                "endpoint": {"type": "string", "description": "CF's OpenAI endpoint, e.g. http://127.0.0.1:7880"},
                "token_file": {"type": "string", "description": "a file with the endpoint's bearer token (cf models token)"},
                "api_token_file": {"type": "string", "description": "a file with a CF API token, when CF asks for one on loopback"},
                "offload": {"type": "string", "enum": ["when_full", "off"]},
                "leave": {"type": "boolean"}
            }),
            &[],
        ),
    ]
}

fn mb(b: u64) -> f64 {
    (b as f64 / 1_048_576.0 * 10.0).round() / 10.0
}

/// The fabric's API: `POST {api}/api/v1/{op}` answers `{"ok": true, "result": ...}`.
async fn cf_call(hub: &Hub, cfg: &FabricConfig, op: &str, args: Value) -> Res<Value> {
    if cfg.api.is_empty() {
        return Err(Fail::invalid("fabric.json has no api address (fabric.set with api)"));
    }
    let mut req = hub
        .engine
        .http
        .post(format!("{}/api/v1/{op}", cfg.api.trim_end_matches('/')))
        .timeout(Duration::from_secs(20))
        .json(&args);
    if let Some(t) = read_token(&cfg.api_token_file) {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Fail::unavailable(format!("the fabric at {} did not answer: {e}", cfg.api)))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| Fail::unavailable(format!("the fabric's answer is not JSON: {e}")))?;
    if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err(Fail::new(
            502,
            "fabric_error",
            v["error"]["message"]
                .as_str()
                .unwrap_or("the fabric refused the operation")
                .to_string(),
        ))
    }
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Res<Value> {
    let e = &hub.engine;
    Ok(match id {
        "machine.status" => {
            use sysinfo::System;
            let st = e.settings();
            let devices = e.gpu_devices_recent().await;
            let mut sys = System::new();
            sys.refresh_memory();
            let loaded = e.loaded();
            json!({
                "gpus": devices.iter().map(|d| json!({"name": d.name, "total_mib": d.total_mib, "free_mib": d.free_mib})).collect::<Vec<_>>(),
                // what a new model may use of the cards: free memory held to the reserve, less what models hold
                "vram_usable_mib": if devices.is_empty() { 0 } else { e.plannable_free(&devices, &st, "") },
                "vram_reserve_mib": st.vram_reserve_mib,
                "ram_total_mb": mb(sys.total_memory()),
                "ram_available_mb": mb(sys.available_memory()),
                "commit_headroom_mib": e.commit_headroom_mib(),
                "loaded": loaded.len(),
                "in_flight": loaded.iter().map(|i| i.inflight()).sum::<usize>(),
            })
        }
        "fabric.status" => {
            let cfg = load(&e.home);
            let Some(c) = cfg else {
                return Ok(json!({"joined": false, "note": "no fabric.json in the hub's home: fabric.set joins one"}));
            };
            let reachable = if c.endpoint.is_empty() {
                Value::Null
            } else {
                json!(e
                    .http
                    .get(format!("{}/health", c.endpoint.trim_end_matches('/')))
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false))
            };
            let node = if c.api.is_empty() {
                Value::Null
            } else {
                cf_call(hub, &c, "models.status", json!({}))
                    .await
                    .unwrap_or(Value::Null)
            };
            json!({"joined": true, "api": c.api, "endpoint": c.endpoint, "offload": c.offload,
                   "token": c.token_file.is_some(), "endpoint_reachable": reachable, "node": node,
                   "offloaded_by_node": offloaded_by_node(&e.home)})
        }
        "fabric.nodes" | "fabric.models" | "fabric.route" => {
            let c = load(&e.home).ok_or_else(|| Fail::invalid("this hub is not joined to a fabric (fabric.set)"))?;
            match id {
                "fabric.nodes" => cf_call(hub, &c, "models.nodes", json!({})).await?,
                "fabric.models" => cf_call(hub, &c, "models.list", json!({})).await?,
                _ => cf_call(hub, &c, "models.route", json!({"model": text(a, "model")?})).await?,
            }
        }
        "fabric.set" => {
            if a["leave"].as_bool().unwrap_or(false) {
                let _ = std::fs::remove_file(e.home.join(FILE));
                return Ok(json!({"joined": false}));
            }
            let mut c = load(&e.home).unwrap_or_default();
            for (key, slot) in [("api", &mut c.api), ("endpoint", &mut c.endpoint)] {
                if let Some(v) = a[key].as_str() {
                    if !(v.is_empty() || v.starts_with("http://") || v.starts_with("https://")) {
                        return Err(Fail::invalid(format!("{key} must be an http:// or https:// address")));
                    }
                    *slot = v.trim_end_matches('/').to_string();
                }
            }
            if let Some(v) = a["token_file"].as_str() {
                c.token_file = (!v.is_empty()).then(|| PathBuf::from(v));
            }
            if let Some(v) = a["api_token_file"].as_str() {
                c.api_token_file = (!v.is_empty()).then(|| PathBuf::from(v));
            }
            if let Some(v) = a["offload"].as_str() {
                c.offload = match v {
                    "off" => Offload::Off,
                    "when_full" => Offload::WhenFull,
                    other => return Err(Fail::invalid(format!("offload is when_full or off, not {other}"))),
                };
            }
            if c.api.is_empty() && c.endpoint.is_empty() {
                return Err(Fail::invalid("give api or endpoint"));
            }
            save(&e.home, &c)?;
            json!({"joined": true, "api": c.api, "endpoint": c.endpoint, "offload": c.offload})
        }
        _ => return Err(Fail::not_found(format!("no operation {id}"))),
    })
}

fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, Fail> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| Fail::invalid(format!("{k} is required")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_without_the_file_is_not_joined() {
        let d = crate::testing::TestDir::new("mm-fabric");
        assert!(load(&d).is_none());
        std::fs::write(d.join(FILE), "{}").unwrap();
        assert!(load(&d).is_none(), "an empty file joins nothing");
        std::fs::write(d.join(FILE), r#"{"endpoint":"http://127.0.0.1:7880","offload":"off"}"#).unwrap();
        let c = load(&d).unwrap();
        assert_eq!(c.offload, Offload::Off);
        assert_eq!(c.endpoint, "http://127.0.0.1:7880");
        std::fs::write(d.join(FILE), "not json").unwrap();
        assert!(load(&d).is_none(), "a broken file joins nothing");
    }

    #[test]
    fn only_a_shortage_of_room_is_a_failure_the_fabric_could_cure() {
        assert!(capacity_failure(&Fail::new(503, "insufficient_memory", "x")));
        assert!(capacity_failure(&Fail::new(503, "insufficient_vram", "x")));
        assert!(capacity_failure(&Fail::new(503, "busy", "x")));
        assert!(!capacity_failure(&Fail::new(404, "not_found", "x")));
        assert!(!capacity_failure(&Fail::new(400, "invalid_request", "x")));
        assert!(!capacity_failure(&Fail::new(403, "model_not_allowed", "x")));
    }

    #[tokio::test]
    async fn the_hop_is_what_the_request_carried() {
        assert_eq!(hop(), 0, "outside a request there is none");
        assert_eq!(HOP.scope(2, async { hop() }).await, 2);
    }
}

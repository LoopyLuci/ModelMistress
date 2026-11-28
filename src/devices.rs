//! Other machines that run ModelMistress, such as the Android app: the owner can send a model from this hub to one of
//! them, from the Models page. The send is a job (progress, stop, restart), and it streams the file, so a model of many
//! gigabytes never sits in memory. The device checks the device's API key, which the owner copies from its Server screen.
//!
//! Only single-file models are sent. A split model (several parts) and an Ollama model are refused for now.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::engine::{Engine, Fail, Result};
use crate::hub::Hub;
use crate::pull::Job;

/// The route a device takes a model on: `POST <url>/mm/v1/models/upload?name=<file name>`, raw bytes, Content-Length set.
pub const UPLOAD_PATH: &str = "/mm/v1/models/upload";
const CHUNK: usize = 1 << 20;

/// One device, as the settings hold it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct DeviceConfig {
    /// Unique among the devices; it names the device in the list and in the job.
    pub name: String,
    /// The device's address with no path: http://host:port (its Server screen shows it).
    pub url: String,
    /// The device's API key. A secret: config.get shows only that it is set.
    pub key: String,
}

/// What the settings may hold for the devices: names unique and not empty, addresses http or https.
pub fn check(devices: &[DeviceConfig]) -> std::result::Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for d in devices {
        let name = d.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err("a device needs a name of 1 to 64 characters".into());
        }
        if !names.insert(name.to_string()) {
            return Err(format!("two devices are named {name:?}; the names must differ"));
        }
        let url = d.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!(
                "device {name:?}: its address must start with http:// or https://"
            ));
        }
    }
    Ok(())
}

/// The devices as config.get shows them: a key is replaced by `(set)`, or by nothing when it is empty.
pub fn shown(devices: &[DeviceConfig]) -> Value {
    json!(devices
        .iter()
        .map(|d| {
            let mut v = json!(d);
            v["key"] = json!(if d.key.is_empty() { "" } else { "(set)" });
            v
        })
        .collect::<Vec<_>>())
}

/// The `devices` list a config.set sends back: a key shown as `(set)` keeps the key the device of that name has now.
pub fn keep_keys(incoming: &Value, current: &[DeviceConfig]) -> Value {
    let Some(list) = incoming.as_array() else {
        return incoming.clone();
    };
    json!(list
        .iter()
        .map(|d| {
            let mut d = d.clone();
            if d.get("key").and_then(Value::as_str) == Some("(set)") {
                let name = d.get("name").and_then(Value::as_str).unwrap_or("");
                let kept = current
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| c.key.clone())
                    .unwrap_or_default();
                d["key"] = json!(kept);
            }
            d
        })
        .collect::<Vec<_>>())
}

/// The address with no trailing slash.
fn base(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// Percent-encodes a file name for a query string: letters, digits and `-_.~` stay as they are.
fn encode(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Streams one model file to a device, as a job. Its progress is the bytes sent; stop it with job.cancel.
/// A send of the same model to the same device while one runs is that job, not a second one.
pub fn send(hub: &Hub, device: DeviceConfig, path: PathBuf, file_name: String) -> Arc<Job> {
    let label = format!("{file_name} to {}", device.name);
    let client = hub.engine.http.clone();
    hub.jobs
        .start_task("send", label, move |job| upload(job, client, device, path, file_name))
}

/// The upload itself. Returns what the device answered (its model id and the bytes sent).
pub async fn upload(
    job: Arc<Job>,
    client: reqwest::Client,
    device: DeviceConfig,
    path: PathBuf,
    file_name: String,
) -> Result<Value> {
    let total = std::fs::metadata(&path)
        .map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))?
        .len();
    job.progress(0, total, format!("sending to {}", device.name));
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| Fail::new(500, "io", format!("{}: {e}", path.display())))?;
    // read in chunks; a stop is seen before each chunk, so the send ends at once
    let stream = futures::stream::unfold((file, job.clone()), |(mut file, job)| async move {
        if job.cancelled() {
            let stopped = std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped");
            return Some((Err(stopped), (file, job)));
        }
        let mut buf = vec![0u8; CHUNK];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                job.done_bytes.fetch_add(n as u64, Ordering::Relaxed);
                Some((Ok(buf), (file, job)))
            }
            Err(e) => Some((Err(e), (file, job))),
        }
    });
    let url = format!("{}{UPLOAD_PATH}?name={}", base(&device.url), encode(&file_name));
    let mut req = client
        .post(&url)
        .header("content-length", total.to_string())
        .header("content-type", "application/octet-stream")
        .body(reqwest::Body::wrap_stream(stream));
    if !device.key.is_empty() {
        req = req.bearer_auth(&device.key);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) if job.cancelled() => {
            return Err(Fail::new(
                499,
                "cancelled",
                "stopped; the device has no complete copy of the model",
            ))
        }
        Err(e) => {
            return Err(Fail::unavailable(format!(
                "{} did not take the model: {}",
                device.name,
                e.without_url()
            )))
        }
    };
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let why = body["error"]["message"].as_str().unwrap_or("no reason given");
        return Err(Fail::new(
            502,
            "device_refused",
            format!("{} refused the model ({status}): {why}", device.name),
        ));
    }
    Ok(json!({"device": device.name, "file": file_name, "bytes": total, "model": body["id"]}))
}

/// The catalog file behind a model id, when it can be sent: a single .gguf file the owner's disk holds.
pub fn sendable(engine: &Engine, id: &str) -> Result<(PathBuf, String)> {
    let entry = engine
        .catalog()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| Fail::not_found(format!("no model {id:?}")))?;
    let name = entry
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    check_sendable(id, entry.source, &name)?;
    Ok((entry.path, name))
}

/// The refusals [`sendable`] makes, from the id, the catalog source and the file name.
fn check_sendable(id: &str, source: &str, name: &str) -> Result<()> {
    if source == "ollama" {
        return Err(Fail::invalid(format!(
            "{id} is an Ollama model, stored as blobs; only .gguf files are sent to a device"
        )));
    }
    if !name.to_lowercase().ends_with(".gguf") {
        return Err(Fail::invalid(format!("{id} is not a .gguf file")));
    }
    let stem = name.strip_suffix(".gguf").unwrap_or(name);
    if crate::catalog::split_part_of(stem).is_some() {
        return Err(Fail::invalid(format!(
            "{id} is split into parts; a split model is not sent to a device yet"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull::Jobs;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use parking_lot::Mutex;
    use std::collections::HashMap;

    /// A stand-in for the device's upload route: it checks the key and records what arrived.
    #[derive(Clone, Default)]
    struct Phone {
        key: String,
        got: Arc<Mutex<Vec<(String, usize, Option<String>)>>>,
    }

    async fn upload_route(
        State(phone): State<Phone>,
        Query(q): Query<HashMap<String, String>>,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> (StatusCode, Json<Value>) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if auth.as_deref() != Some(format!("Bearer {}", phone.key).as_str()) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": {"code": "invalid_api_key", "message": "Send the device's API key."}})),
            );
        }
        let declared = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        phone
            .got
            .lock()
            .push((q.get("name").cloned().unwrap_or_default(), body.len(), declared));
        (StatusCode::OK, Json(json!({"id": "Tiny-Q4", "bytes": body.len()})))
    }

    async fn phone_at(key: &str) -> (Phone, String) {
        let phone = Phone {
            key: key.into(),
            ..Default::default()
        };
        // the stand-in takes a body of any size, as the phone does; axum's default 2 MB limit would close the stream early
        let app = Router::new()
            .route(
                UPLOAD_PATH,
                post(upload_route).layer(axum::extract::DefaultBodyLimit::disable()),
            )
            .with_state(phone.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (phone, format!("http://{addr}/"))
    }

    #[test]
    fn only_a_whole_gguf_file_is_sent_to_a_device() {
        assert!(check_sendable("qwen", "native", "Qwen3-8B-Q4_K_M.gguf").is_ok());
        assert!(check_sendable("qwen", "native", "QWEN3-8B.GGUF").is_ok());
        let blobs = check_sendable("qwen3:8b", "ollama", "sha256-9f2c").unwrap_err();
        assert_eq!(blobs.code, "invalid");
        assert!(blobs.message.contains("Ollama"), "{}", blobs.message);
        let other = check_sendable("tiny", "native", "tiny.safetensors").unwrap_err();
        assert!(other.message.contains("not a .gguf file"), "{}", other.message);
        let split = check_sendable("big", "native", "Big-Q4-00001-of-00003.gguf").unwrap_err();
        assert!(split.message.contains("split into parts"), "{}", split.message);
    }

    fn device(url: &str, key: &str) -> DeviceConfig {
        DeviceConfig {
            name: "phone".into(),
            url: url.into(),
            key: key.into(),
        }
    }

    #[tokio::test]
    async fn a_model_streams_to_the_device_in_full_with_its_length_and_key() {
        let dir = crate::testing::TestDir::new("mm-devices-send");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Tiny-Q4_K_M.gguf");
        // 3 MB and a bit: more than one chunk, so the stream really is read in parts
        let bytes: Vec<u8> = (0..(3 * (1 << 20) + 17)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let (phone, url) = phone_at("KEY123").await;
        let jobs = Arc::new(Jobs::default());
        let job = jobs.start_task("send", "Tiny-Q4_K_M.gguf to phone".into(), {
            let client = reqwest::Client::new();
            let (dev, path) = (device(&url, "KEY123"), path.clone());
            move |job| upload(job, client, dev, path, "Tiny-Q4_K_M.gguf".into())
        });
        while job.status() == "running" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let d = job.describe();
        assert_eq!(d["status"], "done", "{}", d["message"]);
        assert_eq!(d["result"]["model"], "Tiny-Q4", "the device names the model it stored");
        let got = phone.got.lock().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "Tiny-Q4_K_M.gguf");
        assert_eq!(got[0].1, bytes.len(), "every byte arrives");
        assert_eq!(
            got[0].2.as_deref(),
            Some(bytes.len().to_string().as_str()),
            "the length is declared"
        );
        assert_eq!(d["done_bytes"], bytes.len() as u64);
    }

    #[tokio::test]
    async fn a_wrong_key_is_refused_and_the_key_is_not_in_the_message() {
        let dir = crate::testing::TestDir::new("mm-devices-key");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Tiny.gguf");
        std::fs::write(&path, b"GGUF-bytes").unwrap();
        let (_phone, url) = phone_at("RIGHT").await;
        let jobs = Arc::new(Jobs::default());
        let client = reqwest::Client::new();
        let dev = device(&url, "WRONG-KEY");
        let job = jobs.start_task("send", "Tiny.gguf to phone".into(), move |job| {
            upload(job, client, dev, path, "Tiny.gguf".into())
        });
        while job.status() == "running" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let d = job.describe();
        assert_eq!(d["status"], "failed");
        let message = d["message"].as_str().unwrap();
        assert!(message.contains("401") && message.contains("refused"), "{message}");
        assert!(
            !message.contains("WRONG-KEY"),
            "a key never appears in a message: {message}"
        );
    }

    #[test]
    fn names_are_encoded_and_the_settings_are_checked() {
        assert_eq!(encode("Qwen3 8B-Q4_K_M.gguf"), "Qwen3%208B-Q4_K_M.gguf");
        assert_eq!(base("http://192.0.2.10:8080/"), "http://192.0.2.10:8080");
        let good = device("http://192.0.2.10:8080", "KEY");
        assert!(check(std::slice::from_ref(&good)).is_ok());
        assert!(check(&[good.clone(), good.clone()]).unwrap_err().contains("differ"));
        let bad_url = DeviceConfig {
            url: "192.0.2.10:8080".into(),
            ..good.clone()
        };
        assert!(check(&[bad_url]).unwrap_err().contains("http://"));
        let shown_text = shown(std::slice::from_ref(&good)).to_string();
        assert!(!shown_text.contains("KEY"), "{shown_text}");
        let back = keep_keys(&json!([{"name": "phone", "key": "(set)"}]), std::slice::from_ref(&good));
        assert_eq!(back[0]["key"], "KEY");
    }
}

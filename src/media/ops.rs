//! The `media.*` operations and the HTTP routes under `/v1/media`. The owner's: a peer key reaches the model routes only.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use super::ffmpeg::{self, Source, Where};
use super::{caps, probe};
use crate::engine::{Fail, Result};
use crate::hub::{ApiError, Hub};

/// The largest file `POST /v1/media/probe` takes as an upload.
const MAX_UPLOAD: usize = 512 << 20;

fn op(id: &str, summary: &str, mutating: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "media", "summary": summary, "mutating": mutating, "destructive": false,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    vec![
        op(
            "media.components",
            "What the media features need and what is installed: ffmpeg and ffprobe (where they are, version, licence, \
             and the build `media.install` fetches), speech to text (the whisper build and its models), and what each \
             chat model can be given (text, image, audio, video)",
            false,
            json!({}),
            &[],
        ),
        op(
            "media.install",
            "Install ffmpeg and ffprobe (an LGPL build) into <home>/engines/ffmpeg: video and audio input to chat \
             models, speech to text on any container and media.probe need them. Nothing is downloaded without \
             confirm: true; without it the answer says what would be fetched, from where, how big, and its SHA-256. \
             The archive's hash is checked before anything is unpacked and every installed file's hash is checked \
             before it is used. Runs as a job (job.get shows progress) unless wait is true",
            true,
            json!({
                "confirm": {"type": "boolean", "description": "true: yes, install it"},
                "url": {"type": "string", "description": "install the archive at this https address instead of the pinned build (needs sha256)"},
                "file": {"type": "string", "description": "install an archive you downloaded yourself (its sha256 must match the pinned build's, or be given)"},
                "sha256": {"type": "string", "description": "the SHA-256 the archive must have (64 hex digits)"},
                "size": {"type": "integer", "minimum": 1, "description": "the archive's size in bytes, when you know it (progress and a check)"},
                "force": {"type": "boolean", "description": "install again even when this build is already installed and intact"},
                "wait": {"type": "boolean", "description": "answer only when it has finished"}
            }),
            &["confirm"],
        ),
        op(
            "media.probe",
            "What a media file holds, from ffprobe: the container, its duration and size, and every video, audio and \
             subtitle stream (codec, size, frame rate, sample rate, channels, language). The file is on the hub's \
             machine. Needs ffmpeg (media.components)",
            false,
            json!({"file": {"type": "string", "description": "path of the media file"}}),
            &["file"],
        ),
    ]
}

/// Refuses an argument the operation does not have (a typo would otherwise be ignored).
fn only(a: &Value, id: &str, known: &[&str]) -> Result<()> {
    if let Some(k) = a
        .as_object()
        .and_then(|o| o.keys().find(|k| !known.contains(&k.as_str())))
    {
        return Err(Fail::invalid(format!("{id} has no argument {k}")));
    }
    Ok(())
}

fn text<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub async fn call(hub: &Hub, id: &str, a: &Value) -> Result<Value> {
    match id {
        "media.components" => {
            only(a, id, &[])?;
            Ok(components(hub).await)
        }
        "media.install" => install(hub, a).await,
        "media.probe" => {
            only(a, id, &["file"])?;
            let file = text(a, "file")
                .ok_or_else(|| Fail::invalid("file is required"))?
                .to_string();
            probe_file(hub, file).await
        }
        _ => Err(Fail::not_found(format!("no operation {id}"))),
    }
}

/// Everything `media.components` says.
async fn components(hub: &Hub) -> Value {
    let e = &hub.engine;
    let (home, setting) = (e.home.clone(), e.settings().ffmpeg_dir.clone());
    let ff = tokio::task::spawn_blocking(move || ffmpeg::status(&home, setting.as_deref()))
        .await
        .unwrap_or_else(|x| json!({"id": "ffmpeg", "installed": false, "problem": x.to_string()}));
    let video = e.video_state().await;
    let models: Vec<Value> = e
        .catalog()
        .iter()
        .filter(|m| m.mmproj.is_some())
        .map(|m| {
            let mut v = json!({"id": m.id});
            caps::annotate(&mut v, m, video);
            v
        })
        .collect();
    let speech = json!({"id": "whisper", "what": "speech to text (whisper.cpp)", "built": crate::speech::AVAILABLE,
                        "models": crate::speech::list_models(&e.whisper_dirs()).into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
                        "note": (!crate::speech::AVAILABLE).then_some(crate::speech::NOT_BUILT)});
    json!({
        "components": [ff, speech],
        "video_input": {"ready": video == caps::Video::Ready,
                        "why_not": match video {
                            caps::Video::NoFfmpeg => json!(ffmpeg::NOT_INSTALLED),
                            other => json!(other.note()),
                        }},
        "models_with_projector": models,
        "media_folder": super::serve::media_dir(&e.home),
    })
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `url` may be downloaded from: https, or a loopback address (a test server, a mirror on this machine).
fn url_allowed(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    url.strip_prefix("http://").is_some_and(|rest| {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit('@').next().unwrap_or("");
        host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
    })
}

/// The archive to install and its hash: what the caller named, else the pinned build.
fn source_of(a: &Value) -> Result<Source> {
    let (url, file, sha) = (text(a, "url"), text(a, "file"), text(a, "sha256"));
    if let Some(s) = sha {
        if !is_sha256(s) {
            return Err(Fail::invalid("sha256 must be 64 hex digits"));
        }
    }
    let size = match a.get("size") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| Fail::invalid("size must be a whole number of bytes"))?,
    };
    let pinned = Source::pinned();
    match (url, file) {
        (Some(_), Some(_)) => Err(Fail::invalid("give url or file, not both")),
        (Some(u), None) => {
            if !url_allowed(u) {
                return Err(Fail::invalid("url must be an https address"));
            }
            let sha = sha.ok_or_else(|| {
                Fail::invalid("a url needs its sha256: the archive is checked against it before anything is unpacked")
            })?;
            Ok(Source {
                from: Where::Url(u.to_string()),
                sha256: sha.to_lowercase(),
                size,
            })
        }
        (None, Some(f)) => {
            let sha = sha
                .map(str::to_lowercase)
                .or_else(|| pinned.as_ref().map(|p| p.sha256.clone()))
                .ok_or_else(|| {
                    Fail::invalid(format!("no build is pinned for this platform: give the sha256 of {f}"))
                })?;
            Ok(Source {
                from: Where::File(f.into()),
                sha256: sha,
                size,
            })
        }
        (None, None) => {
            if sha.is_some() {
                return Err(Fail::invalid("sha256 goes with url or file"));
            }
            pinned.ok_or_else(|| {
                Fail::invalid(
                    "no ffmpeg build is pinned for this platform: give url and sha256 (an LGPL build), or file and sha256, \
                     or set ffmpeg_dir to a folder that holds ffmpeg and ffprobe",
                )
            })
        }
    }
}

async fn install(hub: &Hub, a: &Value) -> Result<Value> {
    only(
        a,
        "media.install",
        &["confirm", "url", "file", "sha256", "size", "force", "wait"],
    )?;
    let src = source_of(a)?;
    if a["confirm"] != true {
        return Err(Fail::invalid(format!(
            "this installs ffmpeg and ffprobe: it {}; they go into <home>/engines/ffmpeg (an earlier install is kept beside \
             it as ffmpeg.old-<time>). Call again with confirm: true to do it",
            src.describe()
        )));
    }
    let force = a["force"] == true;
    let engine = hub.engine.clone();
    let job = hub
        .jobs
        .start_task("media", "media install: ffmpeg".to_string(), move |job| async move {
            ffmpeg::install(&engine, &job, src, force).await
        });
    if a["wait"] == true {
        while job.status() == "running" {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let d = job.describe();
        if d["status"] != "done" {
            return Err(Fail::new(
                500,
                "setup_failed",
                d["message"].as_str().unwrap_or("the install failed").to_string(),
            ));
        }
        return Ok(d);
    }
    let mut d = job.describe();
    if let Some(o) = d.as_object_mut() {
        o.insert(
            "follow".into(),
            json!("job.get {job} shows progress; media.components says when ffmpeg is ready"),
        );
    }
    Ok(d)
}

/// `media.probe`: ffprobe on a local file.
async fn probe_file(hub: &Hub, file: String) -> Result<Value> {
    let l = hub
        .engine
        .ffmpeg()
        .await
        .map_err(|e| Fail::new(501, "not_implemented", e))?;
    tokio::task::spawn_blocking(move || probe::probe(&l, &file))
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?
        .map_err(Fail::invalid)
}

// ---- routes ------------------------------------------------------------------------------------------------------

pub fn routes() -> Router<Hub> {
    Router::new()
        .route("/v1/media/components", get(components_route))
        .route(
            "/v1/media/probe",
            post(probe_route).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
}

async fn components_route(State(hub): State<Hub>) -> Json<Value> {
    Json(components(&hub).await)
}

/// `POST /v1/media/probe`: JSON `{"file": "<path on the hub's machine>"}`, or a multipart upload with a `file` field.
async fn probe_route(
    State(hub): State<Hub>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<Value>, ApiError> {
    let ctype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("");
    if ctype.to_lowercase().starts_with("multipart/form-data") {
        let up = crate::audio::parse_request(ctype, &body).map_err(Fail::invalid)?;
        let l = hub
            .engine
            .ffmpeg()
            .await
            .map_err(|e| Fail::new(501, "not_implemented", e))?;
        let tmp = super::serve::media_dir(&hub.engine.home).join("tmp");
        let out = tokio::task::spawn_blocking(move || {
            super::with_upload(&tmp, &up.file, &up.filename, |p| {
                let raw = probe::raw(&l, p)?;
                Ok(probe::summarize(&raw, std::path::Path::new(&up.filename)))
            })
        })
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?
        .map_err(Fail::invalid)?;
        return Ok(Json(out));
    }
    let args: Value =
        serde_json::from_slice(&body).map_err(|e| Fail::invalid(format!("the body must be JSON with a file: {e}")))?;
    if !args.is_object() {
        return Err(Fail::invalid("the body must be a JSON object").into());
    }
    only(&args, "media.probe", &["file"])?;
    let file = text(&args, "file")
        .ok_or_else(|| Fail::invalid("file is required"))?
        .to_string();
    Ok(Json(probe_file(&hub, file).await?))
}

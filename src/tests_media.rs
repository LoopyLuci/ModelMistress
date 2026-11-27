//! Media through the hub: `media.components` and `media.install`, and that only the owner reaches them. ffmpeg and ffprobe
//! are stand-in scripts here.

use std::path::{Path, PathBuf};

use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::engine::{Engine, Settings};
use crate::hosting::Quotas;
use crate::hub::{self, Hub};
use crate::media::stub::{pair, zip_archive, Stub};
use crate::testing::{TestDir, TOKEN};

/// A hub with a text model `tiny`. `ffmpeg_dir` and `server` are the settings of the same names.
fn hub_with(home: &Path, ffmpeg_dir: Option<PathBuf>, server: Option<PathBuf>) -> Hub {
    std::fs::create_dir_all(home.join("models")).unwrap();
    crate::catalog::tests_support::gguf(&home.join("models").join("tiny.gguf"), "llama", "tiny");
    Hub::new(
        Engine::new(
            home.to_path_buf(),
            Settings {
                llama_server: Some(server.unwrap_or_else(|| home.join("no-such-llama-server"))),
                ollama_url: String::new(),
                ffmpeg_dir,
                ..Default::default()
            },
        ),
        TOKEN.to_string(),
    )
}

/// A folder with stand-in ffmpeg and ffprobe.
fn tools(dir: &Path) -> PathBuf {
    pair(
        dir,
        &Stub {
            configuration: "--disable-gpl",
            probe_json: "{}",
            pcm: &[],
        },
    )
}

async fn send(h: &Hub, method: &str, path: &str, bearer: &str, body: Option<Value>) -> (StatusCode, Value) {
    let b = match body {
        Some(v) => axum::body::Body::from(v.to_string()),
        None => axum::body::Body::empty(),
    };
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(b)
        .unwrap();
    let resp = hub::router(h.clone()).oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn owner(h: &Hub, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    send(h, method, path, TOKEN, body).await
}

fn message(v: &Value) -> String {
    v["error"]["message"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn the_components_say_what_is_missing_and_what_is_there() {
    let home = TestDir::new("mm-media-comp");
    // a folder with no ffmpeg in it: the setting names it, so PATH is not searched and the answer is the same everywhere
    let empty = home.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let h = hub_with(&home, Some(empty), None);
    let (st, v) = owner(&h, "POST", "/v1/call/media.components", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let r = &v["result"];
    let ff = &r["components"][0];
    assert_eq!(
        (ff["id"].as_str(), ff["installed"].clone()),
        (Some("ffmpeg"), json!(false)),
        "{ff}"
    );
    assert!(ff["problem"].as_str().unwrap().contains("ffmpeg_dir"), "{ff}");
    assert_eq!(r["components"][1]["id"], "whisper");
    // the same answer on the route
    let (st, v2) = owner(&h, "GET", "/v1/media/components", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v2["components"][0]["installed"], false);

    // with ffmpeg and ffprobe in the named folder
    let home2 = TestDir::new("mm-media-comp2");
    let dir = tools(&home2.join("ff"));
    let h = hub_with(&home2, Some(dir), None);
    let (_, v) = owner(&h, "POST", "/v1/call/media.components", Some(json!({}))).await;
    let ff = &v["result"]["components"][0];
    assert_eq!(
        (ff["installed"].clone(), ff["origin"].clone()),
        (json!(true), json!("setting")),
        "{ff}"
    );
    assert_eq!(ff["licence"], "LGPL");
    assert_eq!(ff["version"], "n9.0-stub");
    // a typo in an argument is an error, not an ignored word
    let (st, v) = owner(&h, "POST", "/v1/call/media.components", Some(json!({"verbose": true}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn nothing_is_installed_without_a_yes_a_source_and_a_hash() {
    let home = TestDir::new("mm-media-install");
    let h = hub_with(&home, None, None);
    let sha = "ab".repeat(32);
    // no yes: the answer says what would be fetched, from where, and the hash it must have
    let (st, v) = owner(
        &h,
        "POST",
        "/v1/call/media.install",
        Some(json!({"confirm": false, "url": "https://example.org/dl/ffmpeg-lgpl.zip", "sha256": sha, "size": 164000000})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let m = message(&v);
    for want in ["ffmpeg-lgpl.zip", "example.org", "164 MB", &sha, "confirm: true"] {
        assert!(m.contains(want), "{want} not in {m}");
    }
    // a url with no hash, a plain http url, a bad hash, and (no build is pinned here yet) nothing at all
    for (args, word) in [
        (json!({"confirm": true, "url": "https://example.org/a.zip"}), "sha256"),
        (
            json!({"confirm": true, "url": "http://example.org/a.zip", "sha256": sha}),
            "https",
        ),
        (
            json!({"confirm": true, "url": "https://example.org/a.zip", "sha256": "xyz"}),
            "64 hex",
        ),
        (json!({"confirm": true}), "pinned"),
        (
            json!({"confirm": true, "url": "https://example.org/a.zip", "file": "x", "sha256": sha}),
            "not both",
        ),
    ] {
        let (st, v) = owner(&h, "POST", "/v1/call/media.install", Some(args.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{args}: {v}");
        assert!(message(&v).contains(word), "{word} not in {}", message(&v));
    }
    assert!(!home.join("engines").join("ffmpeg").exists());
    assert!(!home.join("engines").join("_downloads").exists(), "nothing was fetched");
}

#[tokio::test]
async fn an_archive_the_owner_names_installs_when_its_hash_matches() {
    let work = TestDir::new("mm-media-file");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = hub_with(&home, None, None);
    let (archive, sha) = zip_archive(&work, "--disable-gpl --enable-version3");
    let file = archive.to_string_lossy().to_string();

    let (st, v) = owner(
        &h,
        "POST",
        "/v1/call/media.install",
        Some(json!({"confirm": true, "file": file, "sha256": "0".repeat(64), "wait": true})),
    )
    .await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
    assert!(message(&v).contains("nothing was installed"), "{v}");
    assert!(!home.join("engines").join("ffmpeg").exists());

    let (st, v) = owner(
        &h,
        "POST",
        "/v1/call/media.install",
        Some(json!({"confirm": true, "file": file, "sha256": sha, "wait": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["result"]["status"], "done", "{v}");
    let (_, v) = owner(&h, "POST", "/v1/call/media.components", Some(json!({}))).await;
    let ff = &v["result"]["components"][0];
    assert_eq!(
        (ff["installed"].clone(), ff["origin"].clone()),
        (json!(true), json!("installed")),
        "{ff}"
    );
    assert_eq!(ff["licence"], "LGPL");
    assert!(archive.is_file(), "the owner's own file is left alone");
}

#[tokio::test]
async fn only_the_owner_reaches_the_media_operations() {
    let home = TestDir::new("mm-media-peer");
    let h = hub_with(&home, None, None);
    let (_, secret) = h.keys.create("peer", Quotas::default(), vec![], "hub").unwrap();
    for (method, path) in [
        ("POST", "/v1/call/media.install"),
        ("POST", "/v1/call/media.components"),
        ("POST", "/v1/call/media.probe"),
        ("GET", "/v1/media/components"),
        ("POST", "/v1/media/probe"),
    ] {
        let (st, v) = send(&h, method, path, &secret, Some(json!({}))).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{path}: {v}");
    }
}

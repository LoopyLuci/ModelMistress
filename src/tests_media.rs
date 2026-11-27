//! Media through the hub: `media.components` and `media.install`, what the model listings say a model can be given, and
//! how a chat request with a video, audio or image part reaches the server (the owner's local files staged under the
//! media folder, a peer's request untouched). ffmpeg, ffprobe and llama-server are stand-in scripts here.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::engine::{Engine, Settings};
use crate::hosting::Quotas;
use crate::hub::{self, Hub};
use crate::media::stub::{pair, server_stub, zip_archive, Stub};
use crate::testing::{recording_stand_in, TestDir, TOKEN};

/// A hub with a text model `tiny` and a vision model `seer` (its projector beside it). `ffmpeg_dir` and `server` are
/// the settings of the same names.
fn hub_with(home: &Path, ffmpeg_dir: Option<PathBuf>, server: Option<PathBuf>) -> Hub {
    use crate::catalog::tests_support::{gguf, gguf_with};
    std::fs::create_dir_all(home.join("models").join("seer")).unwrap();
    gguf(&home.join("models").join("tiny.gguf"), "llama", "tiny");
    gguf(&home.join("models").join("seer").join("seer.gguf"), "llama", "seer");
    gguf_with(
        &home.join("models").join("seer").join("mmproj-F16.gguf"),
        "clip",
        "proj",
        &[("clip.has_vision_encoder", true)],
    );
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

fn file_url(p: &Path) -> String {
    let fwd = p.to_string_lossy().replace('\\', "/");
    if fwd.starts_with('/') {
        format!("file://{fwd}")
    } else {
        format!("file:///{fwd}")
    }
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
    assert_eq!(r["video_input"]["ready"], false);
    assert_eq!(r["any_container_transcription"]["ready"], false);
    assert_eq!(r["components"][1]["id"], "whisper");
    let seer = r["models_with_projector"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "seer")
        .unwrap();
    assert_eq!(seer["input_modalities"], json!(["text", "image"]));
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

#[tokio::test]
async fn the_listings_say_what_each_model_can_be_given() {
    let work = TestDir::new("mm-media-list");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let empty = work.join("none");
    std::fs::create_dir_all(&empty).unwrap();
    let find = |v: &Value, id: &str| v.as_array().unwrap().iter().find(|m| m["id"] == id).unwrap().clone();

    // no ffmpeg: images, and a note about why video is missing
    let h = hub_with(&home, Some(empty), None);
    let (_, v) = owner(&h, "GET", "/v1/models", None).await;
    let (seer, tiny) = (find(&v["data"], "seer"), find(&v["data"], "tiny"));
    assert_eq!(seer["input_modalities"], json!(["text", "image"]), "{seer}");
    assert!(seer["video_note"].as_str().unwrap().contains("media.install"), "{seer}");
    assert_eq!(tiny["input_modalities"], json!(["text"]));
    assert!(tiny.get("video_note").is_none());
    let (_, v) = owner(&h, "POST", "/v1/call/model.list", Some(json!({}))).await;
    assert_eq!(find(&v["result"], "seer")["input_modalities"], json!(["text", "image"]));

    // ffmpeg, and a llama-server that lists --video-ffmpeg-dir: video too
    let ff = tools(&work.join("ff"));
    let server = server_stub(&work.join("srv"), &["--media-path", "--video-ffmpeg-dir"]);
    let h = hub_with(&home, Some(ff.clone()), Some(server));
    let (_, v) = owner(&h, "GET", "/v1/models", None).await;
    let seer = find(&v["data"], "seer");
    assert_eq!(seer["input_modalities"], json!(["text", "image", "video"]), "{seer}");
    assert!(seer.get("video_note").is_none());
    let (_, v) = owner(&h, "POST", "/v1/call/media.components", Some(json!({}))).await;
    assert_eq!(v["result"]["video_input"]["ready"], true, "{v}");

    // an older llama-server does not take it: the note says which flag it lacks
    let old = server_stub(&work.join("old"), &["--media-path"]);
    let h = hub_with(&home, Some(ff), Some(old));
    let (_, v) = owner(&h, "GET", "/v1/models", None).await;
    let seer = find(&v["data"], "seer");
    assert_eq!(seer["input_modalities"], json!(["text", "image"]));
    assert!(
        seer["video_note"].as_str().unwrap().contains("--video-ffmpeg-dir"),
        "{seer}"
    );
}

/// A chat to `seer` with a text part, a video part and an audio part.
fn chat_with(video: &str, audio: &str) -> Value {
    json!({"model": "seer", "stream": false, "messages": [{"role": "user", "content": [
        {"type": "text", "text": "what is said?"},
        {"type": "video_url", "video_url": {"url": video}},
        {"type": "input_audio", "input_audio": {"data": audio, "format": "wav"}},
        {"type": "image_url", "image_url": {"url": "https://example.org/a.png"}}]}]})
}

#[tokio::test]
async fn a_chat_with_media_reaches_the_server_the_way_it_reads_files() {
    let work = TestDir::new("mm-media-chat");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let clip = work.join("my clip.mp4");
    let talk = work.join("talk.wav");
    std::fs::write(&clip, b"video bytes").unwrap();
    std::fs::write(&talk, b"audio bytes").unwrap();
    let h = hub_with(&home, None, None);
    let (server, bodies) = recording_stand_in("ok").await;
    h.engine
        .adopt_as("seer", server.port, "k", false, Some(server.on_stop()));

    // the owner names files on this machine: the server gets `file://staged/...` names of copies under <home>/media
    let (st, v) = owner(
        &h,
        "POST",
        "/v1/chat/completions",
        Some(chat_with(&file_url(&clip), &talk.to_string_lossy())),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let got = bodies.lock().unwrap()[0]["messages"][0]["content"].clone();
    let (video, audio) = (
        got[1]["video_url"]["url"].as_str().unwrap().to_string(),
        got[2]["input_audio"]["data"].as_str().unwrap().to_string(),
    );
    for u in [&video, &audio] {
        let name = u.strip_prefix("file://staged/").unwrap_or_else(|| panic!("{u}"));
        assert!(home.join("media").join("staged").join(name).is_file(), "{u}");
    }
    assert!(video.ends_with("-my_clip.mp4"), "{video}");
    assert_eq!(got[0]["text"], "what is said?");
    assert_eq!(got[2]["input_audio"]["format"], "wav");
    assert_eq!(
        got[3]["image_url"]["url"], "https://example.org/a.png",
        "a web address is the server's to fetch"
    );

    // a name under the media folder, base64 and the like go through as they are
    let (st, _) = owner(
        &h,
        "POST",
        "/v1/chat/completions",
        Some(chat_with("file://clip.mp4", "data:audio/wav;base64,AAAA")),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let got = bodies.lock().unwrap()[1]["messages"][0]["content"].clone();
    assert_eq!(got[1]["video_url"]["url"], "file://clip.mp4");
    assert_eq!(got[2]["input_audio"]["data"], "data:audio/wav;base64,AAAA");

    // a file that is not there is refused here, naming it; the server is not asked
    let before = server.seen.load(Ordering::SeqCst);
    let gone = work.join("gone.mp4");
    let (st, v) = owner(
        &h,
        "POST",
        "/v1/chat/completions",
        Some(chat_with(&file_url(&gone), "x")),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(message(&v).contains("gone.mp4"), "{v}");
    assert_eq!(server.seen.load(Ordering::SeqCst), before);

    // the owner's operation does the same
    let (st, v) = owner(
        &h,
        "POST",
        "/v1/call/chat.complete",
        Some(json!({"model": "seer", "messages": [{"role": "user", "content": [
            {"type": "video_url", "video_url": {"url": file_url(&clip)}}]}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let last = bodies.lock().unwrap().last().unwrap()["messages"][0]["content"][0]["video_url"]["url"].clone();
    assert!(last.as_str().unwrap().starts_with("file://staged/"), "{last}");
}

#[tokio::test]
async fn a_peers_media_is_never_rewritten_to_reach_a_file_of_this_machine() {
    let work = TestDir::new("mm-media-peerchat");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let secret_file = work.join("secret.mp4");
    std::fs::write(&secret_file, b"not for peers").unwrap();
    let h = hub_with(&home, None, None);
    let (_, key) = h
        .keys
        .create("peer", Quotas::default(), vec!["seer".into()], "hub")
        .unwrap();
    let (server, bodies) = recording_stand_in("ok").await;
    h.engine
        .adopt_as("seer", server.port, "k", false, Some(server.on_stop()));

    let asked = file_url(&secret_file);
    let (st, v) = send(&h, "POST", "/v1/chat/completions", &key, Some(chat_with(&asked, "x"))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let url = bodies.lock().unwrap()[0]["messages"][0]["content"][1]["video_url"]["url"].clone();
    assert_eq!(
        url,
        json!(asked),
        "the peer's request goes to the server as it was sent"
    );
    assert!(
        !home.join("media").join("staged").exists(),
        "nothing of the peer's was copied into the media folder"
    );
}

#[tokio::test]
async fn a_chat_may_carry_a_clip_as_base64() {
    let work = TestDir::new("mm-media-big");
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = hub_with(&home, None, None);
    let (server, bodies) = recording_stand_in("ok").await;
    h.engine
        .adopt_as("seer", server.port, "k", false, Some(server.on_stop()));
    // 3 MB of base64 is over axum's own limit of 2 MB, which refused a short clip
    let clip = format!("data:video/mp4;base64,{}", "A".repeat(3 << 20));
    let (st, v) = owner(&h, "POST", "/v1/chat/completions", Some(chat_with(&clip, "x"))).await;
    assert_eq!(st, StatusCode::OK, "{}", &v.to_string()[..v.to_string().len().min(300)]);
    assert_eq!(
        bodies.lock().unwrap()[0]["messages"][0]["content"][1]["video_url"]["url"]
            .as_str()
            .unwrap()
            .len(),
        clip.len()
    );
}

//! What a model can be given: text, images, audio, video. The projector file (`mmproj*.gguf`) says whether the model has a
//! vision encoder and an audio encoder; video is frames of a file through the vision encoder, so it needs the vision
//! encoder and a video decoder (ffmpeg, which llama-server runs) on top.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::catalog::{read_meta, ModelEntry};

/// Whether the server of a model can decode video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Video {
    /// ffmpeg is there and the server takes `--video-ffmpeg-dir`.
    Ready,
    /// ffmpeg is not installed: `media.install`.
    NoFfmpeg,
    /// The llama-server does not list `--video-ffmpeg-dir`: it is older than video input.
    ServerTooOld,
    /// No llama-server was found at all.
    NoServer,
}

impl Video {
    /// Why video is not ready (None when it is).
    pub fn note(self) -> Option<&'static str> {
        match self {
            Video::Ready => None,
            Video::NoFfmpeg => Some("video input needs ffmpeg: media.install (confirm: true) installs it"),
            Video::ServerTooOld => Some("this llama-server is too old for video input (it has no --video-ffmpeg-dir)"),
            Video::NoServer => Some("no llama-server was found, so no model can be served"),
        }
    }
}

/// What the projector file holds: (vision encoder, audio encoder).
pub fn projector(path: &Path) -> (bool, bool) {
    // (file size, modified time) and what the file said then
    type Entry = ((u64, Option<SystemTime>), (bool, bool));
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    let key = std::fs::metadata(path).map(|m| (m.len(), m.modified().ok())).ok();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(k) = key {
        if let Some((have, v)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path) {
            if *have == k {
                return *v;
            }
        }
    }
    let meta = read_meta(path).unwrap_or_default();
    let flag = |k: &str, default: bool| meta.get(k).and_then(Value::as_bool).unwrap_or(default);
    // a projector that does not say is a vision projector, as the first ones were
    let v = (
        flag("clip.has_vision_encoder", true),
        flag("clip.has_audio_encoder", false),
    );
    if let Some(k) = key {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf(), (k, v));
    }
    v
}

/// What a model takes, in llama-server's order (`text`, `image`, `audio`, `video`), and why video is missing when it is.
pub fn input_modalities(entry: &ModelEntry, video: Video) -> (Vec<&'static str>, Option<&'static str>) {
    let mut v = vec!["text"];
    let Some(p) = &entry.mmproj else { return (v, None) };
    let (vision, audio) = projector(p);
    if vision {
        v.push("image");
    }
    if audio {
        v.push("audio");
    }
    let mut note = None;
    if vision {
        if video == Video::Ready {
            v.push("video");
        } else {
            note = video.note();
        }
    }
    (v, note)
}

/// Adds `input_modalities` (and `video_note`, when video is possible for the model but not ready) to a model's listing.
pub fn annotate(listing: &mut Value, entry: &ModelEntry, video: Video) {
    let (modalities, note) = input_modalities(entry, video);
    listing["input_modalities"] = json!(modalities);
    if let Some(n) = note {
        listing["video_note"] = json!(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests_support::gguf;
    use crate::testing::TestDir;

    fn entry(mmproj: Option<PathBuf>) -> ModelEntry {
        ModelEntry {
            id: "m".into(),
            source: "native",
            path: PathBuf::from("m.gguf"),
            size_bytes: 1,
            mmproj,
            meta: Default::default(),
            native: None,
        }
    }

    #[test]
    fn a_plain_model_takes_text_only() {
        let (m, note) = input_modalities(&entry(None), Video::Ready);
        assert_eq!(m, ["text"]);
        assert!(note.is_none());
    }

    #[test]
    fn the_projector_decides_image_and_audio_and_ffmpeg_decides_video() {
        let dir = TestDir::new("mm-caps");
        // a projector the test writer makes says nothing about its encoders: a vision projector, as the first ones were
        let p = dir.join("mmproj-F16.gguf");
        gguf(&p, "clip", "proj");
        assert_eq!(projector(&p), (true, false));
        let e = entry(Some(p));
        assert_eq!(input_modalities(&e, Video::Ready).0, ["text", "image", "video"]);
        let (m, note) = input_modalities(&e, Video::NoFfmpeg);
        assert_eq!(m, ["text", "image"]);
        assert!(note.unwrap().contains("media.install"));
        let (m, note) = input_modalities(&e, Video::ServerTooOld);
        assert_eq!(m, ["text", "image"]);
        assert!(note.unwrap().contains("--video-ffmpeg-dir"));
        let mut v = json!({"id": "m"});
        annotate(&mut v, &e, Video::NoFfmpeg);
        assert_eq!(v["input_modalities"], json!(["text", "image"]));
        assert!(v["video_note"].as_str().is_some());
        let mut v = json!({"id": "m"});
        annotate(&mut v, &e, Video::Ready);
        assert!(v.get("video_note").is_none());
    }

    #[test]
    fn a_projector_that_hears_is_listed_as_taking_audio() {
        let dir = TestDir::new("mm-caps-audio");
        let p = dir.join("mmproj-audio.gguf");
        crate::catalog::tests_support::gguf_with(
            &p,
            "clip",
            "proj",
            &[("clip.has_vision_encoder", false), ("clip.has_audio_encoder", true)],
        );
        assert_eq!(projector(&p), (false, true));
        let e = entry(Some(p));
        // audio only: no image, so no video either, and no note about video
        let (m, note) = input_modalities(&e, Video::NoFfmpeg);
        assert_eq!(m, ["text", "audio"]);
        assert!(note.is_none());
        let both = dir.join("mmproj-both.gguf");
        crate::catalog::tests_support::gguf_with(
            &both,
            "clip",
            "proj",
            &[("clip.has_vision_encoder", true), ("clip.has_audio_encoder", true)],
        );
        assert_eq!(
            input_modalities(&entry(Some(both)), Video::Ready).0,
            ["text", "image", "audio", "video"]
        );
    }
}

//! What llama-server needs for media, and how a local file reaches it.
//!
//! A server reads a local file only as `file://<name>` under the folder it was started with (`--media-path`): a path it
//! is given any other way is read as base64 and fails (measured: HTTP 500). The hub starts every server that has a
//! projector with `--media-path <home>/media`. The owner's own requests may name a file anywhere on the machine: the hub
//! links or copies it into `<home>/media/staged/` and hands the server the `file://` name of the copy. A peer key's
//! request is never rewritten: it can name only what is already in `<home>/media`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::engine::Fail;

/// The folder llama-server reads `file://` names under: `<home>/media`.
pub fn media_dir(home: &Path) -> PathBuf {
    home.join("media")
}

/// Where copies of the owner's files are kept (inside the media folder, so the server can read them).
const STAGED: &str = "staged";

/// Staged copies older than this are removed the next time a file is staged.
const KEEP: Duration = Duration::from_secs(24 * 3600);

/// Frames per second llama-server samples from a video unless the owner says otherwise.
pub const VIDEO_FPS: &str = "1";

/// The arguments for a model with a projector. `media_dir` and `ffmpeg_dir` are given only where the server lists the
/// flag (and the folder is there); a flag the owner's own arguments already set is left to them.
///
/// With ffmpeg, a server that lists `--video-fps` is also given [`VIDEO_FPS`]: its own default sampled a 32 s clip into
/// 52,595 tokens, over a 32k context, where one frame a second took 13,116. The owner's `--video-fps` wins.
pub fn args(media_dir: Option<&Path>, ffmpeg_dir: Option<&Path>, has_fps: bool, owner: &[String]) -> Vec<String> {
    let set = |flag: &str| owner.iter().any(|a| a == flag || a.starts_with(&format!("{flag}=")));
    let mut out = vec![];
    if let (Some(d), false) = (media_dir, set("--media-path")) {
        out.extend(["--media-path".to_string(), d.display().to_string()]);
    }
    if let (Some(d), false) = (ffmpeg_dir, set("--video-ffmpeg-dir")) {
        out.extend(["--video-ffmpeg-dir".to_string(), d.display().to_string()]);
    }
    if ffmpeg_dir.is_some() && has_fps && !set("--video-fps") {
        out.extend(["--video-fps".to_string(), VIDEO_FPS.to_string()]);
    }
    out
}

/// The URL inside a media part, and a way to put another one in. The parts are OpenAI's `image_url` and
/// `video_url`, and llama-server's `input_video` and `input_audio`.
fn part_url(part: &mut Value) -> Option<&mut Value> {
    let kind = part.get("type")?.as_str()?.to_string();
    let key = match kind.as_str() {
        "image_url" | "video_url" | "input_video" | "input_audio" => kind.as_str(),
        _ => return None,
    };
    let inner = part.get_mut(key)?;
    if inner.is_string() {
        return Some(inner);
    }
    // `url` is the usual key; llama-server also reads `data` from an input_video and an input_audio
    let field = if inner.get("url").is_some_and(Value::is_string) {
        "url"
    } else {
        "data"
    };
    inner.get_mut(field).filter(|v| v.is_string())
}

/// `%20` and the like decoded (bytes that are not valid UTF-8 are replaced); text with no `%` is returned as it is.
fn percent_decoded(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b[i] == b'%' && i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() {
            out.push(hex(b[i + 1]).unwrap_or(0) * 16 + hex(b[i + 2]).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The path a URL names when it is a local absolute one: `file:///C:/clips/a.mp4`, `file:///home/me/a.mp4` or a bare
/// absolute path. `file://a.mp4` (a name under the media folder), `http`, `data:` and base64 are not local paths.
pub fn local_path(url: &str) -> Option<PathBuf> {
    let url = url.trim();
    let raw = match url.strip_prefix("file://") {
        Some(rest) => {
            // file:///C:/x  ->  C:/x ;  file:///home/x  ->  /home/x ; `%20` is a space, as in any file URL
            let rest = percent_decoded(rest).replace('\\', "/");
            let drive = rest.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':'));
            match drive {
                Some(d) => d.to_string(),
                None => rest,
            }
        }
        None => url.to_string(),
    };
    let looks_absolute = {
        let b = raw.as_bytes();
        raw.starts_with('/')
            || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
    };
    (looks_absolute && !raw.contains("://")).then(|| PathBuf::from(raw))
}

fn safe_name(p: &Path) -> String {
    let n: String = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let n = n.trim_start_matches('.').to_string();
    if n.is_empty() {
        "media".to_string()
    } else {
        n.chars().take(80).collect()
    }
}

/// Links (else copies) `source` into `<media>/staged/` and returns the `file://` name of the copy. The same file is
/// staged once: its name carries a hash of its path, size and time.
pub fn stage(media: &Path, source: &Path) -> Result<String, String> {
    let md = std::fs::metadata(source).map_err(|e| format!("{}: {e}", source.display()))?;
    if !md.is_file() {
        return Err(format!("{} is not a file", source.display()));
    }
    let mut h = Sha256::new();
    h.update(source.to_string_lossy().as_bytes());
    h.update(md.len().to_le_bytes());
    if let Some(t) = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
    {
        h.update(t.as_secs().to_le_bytes());
    }
    let key: String = h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect();
    let name = format!("{key}-{}", safe_name(source));
    let dir = media.join(STAGED);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    sweep(&dir);
    let dest = dir.join(&name);
    if !dest.is_file() {
        let part = dir.join(format!("{name}.part"));
        let _ = std::fs::remove_file(&part);
        if std::fs::hard_link(source, &part).is_err() {
            std::fs::copy(source, &part).map_err(|e| format!("{}: {e}", part.display()))?;
        }
        std::fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(format!("file://{STAGED}/{name}"))
}

/// Removes staged files that nobody asked for in a day.
fn sweep(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if old && e.path().is_file() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Stages every local file the request's media parts name, and puts the `file://` name of the copy in its place. Returns
/// how many were staged. Blocking. Call it for the owner's requests only.
pub fn stage_request(body: &mut Value, media: &Path) -> Result<usize, String> {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return Ok(0);
    };
    let mut n = 0;
    for m in messages {
        let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for p in parts {
            let Some(slot) = part_url(p) else { continue };
            let Some(path) = slot.as_str().and_then(local_path) else {
                continue;
            };
            *slot = Value::String(stage(media, &path)?);
            n += 1;
        }
    }
    Ok(n)
}

/// The owner's request with its local files staged (see [`stage_request`]). A request with no media part is returned as it
/// is, and a file that cannot be staged is an error that names it.
pub async fn stage_for_owner(mut body: Value, home: &Path) -> Result<Value, Fail> {
    if !has_media(&body) {
        return Ok(body);
    }
    let media = media_dir(home);
    tokio::task::spawn_blocking(move || stage_request(&mut body, &media).map(|_| body))
        .await
        .map_err(|e| Fail::new(500, "io", e.to_string()))?
        .map_err(Fail::invalid)
}

/// Whether the request has a media part at all (so a request of text only skips the work).
pub fn has_media(body: &Value) -> bool {
    body.get("messages").and_then(Value::as_array).is_some_and(|ms| {
        ms.iter().any(|m| {
            m.get("content").and_then(Value::as_array).is_some_and(|parts| {
                parts.iter().any(|p| {
                    matches!(
                        p.get("type").and_then(Value::as_str),
                        Some("image_url" | "video_url" | "input_video" | "input_audio")
                    )
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDir;
    use serde_json::json;

    #[test]
    fn the_arguments_name_the_folders_and_leave_the_owners_flags_alone() {
        let (m, f) = (Path::new("/h/media"), Path::new("/h/engines/ffmpeg"));
        assert_eq!(
            args(Some(m), Some(f), false, &[]),
            ["--media-path", "/h/media", "--video-ffmpeg-dir", "/h/engines/ffmpeg"]
        );
        // a server that lacks a flag is given none; no ffmpeg, no ffmpeg flag
        assert_eq!(args(Some(m), None, true, &[]), ["--media-path", "/h/media"]);
        assert!(args(None, None, true, &[]).is_empty());
        // the owner's own arguments win, with the flag and value apart or joined
        let own = [
            "--media-path".to_string(),
            "/mine".to_string(),
            "--video-ffmpeg-dir=/ff".to_string(),
        ];
        assert_eq!(args(Some(m), Some(f), true, &own), ["--video-fps", "1"]);
        assert!(args(Some(m), Some(f), false, &own).is_empty());
        let fps = ["--video-fps".to_string(), "4".to_string()];
        assert_eq!(args(None, Some(f), true, &fps), ["--video-ffmpeg-dir", "/h/engines/ffmpeg"]);
        assert!(args(None, None, true, &[]).is_empty(), "no ffmpeg, no frame rate");
        assert_eq!(
            args(Some(m), Some(f), false, &["--media-path=/x".to_string()]),
            ["--video-ffmpeg-dir", "/h/engines/ffmpeg"]
        );
    }

    #[test]
    fn local_paths_are_told_from_names_urls_and_base64() {
        for local in [
            "file:///C:/clips/a.mp4",
            "file://C:\\clips\\a.mp4",
            "C:\\clips\\a.mp4",
            "C:/clips/a.mp4",
            "/home/me/a.mp4",
            "file:///home/me/a.mp4",
        ] {
            assert!(local_path(local).is_some(), "{local}");
        }
        assert_eq!(
            local_path("file:///C:/clips/a.mp4").unwrap(),
            PathBuf::from("C:/clips/a.mp4")
        );
        assert_eq!(
            local_path("file:///home/me/a.mp4").unwrap(),
            PathBuf::from("/home/me/a.mp4")
        );
        for other in [
            "file://clip.mp4",
            "file://staged/abc-clip.mp4",
            "https://example.org/a.mp4",
            "http://localhost:8080/a.mp4",
            "data:video/mp4;base64,AAAA",
            "AAAABBBBCCCC",
            "",
        ] {
            assert!(local_path(other).is_none(), "{other}");
        }
    }

    #[test]
    fn file_urls_are_decoded_but_plain_paths_are_not() {
        assert_eq!(
            local_path("file:///C:/my%20clips/a%2Bb.mp4").unwrap(),
            PathBuf::from("C:/my clips/a+b.mp4")
        );
        assert_eq!(
            local_path("file:///home/me/caf%C3%A9.mp4").unwrap(),
            PathBuf::from("/home/me/café.mp4")
        );
        // a bare path is a path: its `%` stays
        assert_eq!(
            local_path("/home/me/100%.mp4").unwrap(),
            PathBuf::from("/home/me/100%.mp4")
        );
        assert_eq!(percent_decoded("a%2"), "a%2");
        assert_eq!(percent_decoded("a%zzb%41"), "a%zzbA");
    }

    #[test]
    fn a_local_file_is_staged_once_under_a_safe_name() {
        let dir = TestDir::new("mm-stage");
        let media = dir.join("media");
        let src = dir.join("my clip (1).mp4");
        std::fs::write(&src, b"video bytes").unwrap();
        let a = stage(&media, &src).unwrap();
        let b = stage(&media, &src).unwrap();
        assert_eq!(a, b, "the same file is staged once");
        let name = a.strip_prefix("file://staged/").unwrap();
        assert!(
            name.ends_with("-my_clip__1_.mp4") && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "{name}"
        );
        assert_eq!(std::fs::read(media.join("staged").join(name)).unwrap(), b"video bytes");
        assert_eq!(std::fs::read_dir(media.join("staged")).unwrap().count(), 1);
        // a changed file is a new copy
        std::fs::write(&src, b"longer video bytes").unwrap();
        assert_ne!(stage(&media, &src).unwrap(), a);
        assert!(stage(&media, &dir.join("none.mp4")).is_err());
        assert!(stage(&media, &dir).unwrap_err().contains("not a file"));
    }

    #[test]
    fn old_staged_files_are_swept() {
        let dir = TestDir::new("mm-stage-sweep");
        let media = dir.join("media");
        let staged = media.join("staged");
        std::fs::create_dir_all(&staged).unwrap();
        let old = staged.join("old.mp4");
        std::fs::write(&old, b"x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&old).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(3 * 24 * 3600))
            .unwrap();
        drop(f);
        let src = dir.join("new.mp4");
        std::fs::write(&src, b"y").unwrap();
        stage(&media, &src).unwrap();
        assert!(!old.exists(), "a copy nobody asked for in a day is removed");
    }

    #[test]
    fn a_request_has_its_local_files_replaced_and_nothing_else_touched() {
        let dir = TestDir::new("mm-stage-req");
        let media = dir.join("media");
        let clip = dir.join("clip.mp4");
        let wav = dir.join("talk.wav");
        std::fs::write(&clip, b"v").unwrap();
        std::fs::write(&wav, b"a").unwrap();
        let fwd = |p: &Path| p.to_string_lossy().replace('\\', "/");
        let mut body = json!({"model": "m", "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "what happens?"},
                {"type": "video_url", "video_url": {"url": format!("file:///{}", fwd(&clip))}},
                {"type": "input_audio", "input_audio": {"data": wav.display().to_string(), "format": "wav"}},
                {"type": "video_url", "video_url": {"url": "file://already.mp4"}},
                {"type": "image_url", "image_url": {"url": "https://example.org/a.png"}},
                {"type": "input_video", "input_video": "data:video/mp4;base64,AAAA"}
            ]},
            {"role": "assistant", "content": "plain text stays"}]});
        assert!(has_media(&body));
        let n = stage_request(&mut body, &media).unwrap();
        assert_eq!(n, 2);
        let parts = &body["messages"][0]["content"];
        assert!(
            parts[1]["video_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("file://staged/"),
            "{parts}"
        );
        assert!(parts[2]["input_audio"]["data"]
            .as_str()
            .unwrap()
            .starts_with("file://staged/"));
        assert_eq!(parts[2]["input_audio"]["format"], "wav");
        assert_eq!(parts[3]["video_url"]["url"], "file://already.mp4");
        assert_eq!(parts[4]["image_url"]["url"], "https://example.org/a.png");
        assert_eq!(parts[5]["input_video"], "data:video/mp4;base64,AAAA");
        assert_eq!(body["messages"][1]["content"], "plain text stays");
        // a file that is not there is an error that names it, not a request that fails inside the server
        let mut bad = json!({"messages": [{"role": "user", "content": [
            {"type": "video_url", "video_url": {"url": format!("file:///{}", fwd(&dir.join("gone.mp4")))}}]}]});
        assert!(stage_request(&mut bad, &media).unwrap_err().contains("gone.mp4"));
        assert!(!has_media(&json!({"messages": [{"role": "user", "content": "hi"}]})));
        assert_eq!(stage_request(&mut json!({"prompt": "x"}), &media).unwrap(), 0);
    }
}

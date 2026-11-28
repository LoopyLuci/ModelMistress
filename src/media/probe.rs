//! `media.probe`: what a media file holds (container, streams, duration), from ffprobe's JSON.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::ffmpeg::Located;
use super::{last_line, run};

/// How long ffprobe may take for one file.
const TIMEOUT_SECS: u64 = 60;
/// The most JSON ffprobe may print (a file with thousands of streams or chapters is not a media file).
const MAX_JSON: usize = 8 << 20;

/// The absolute form of `path`, as ffmpeg is given it: a file the caller may read, not a URL or a device.
pub fn local_file(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path.trim());
    if path.trim().is_empty() {
        return Err("file is required".into());
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(p)
    };
    let md = std::fs::metadata(&abs).map_err(|e| format!("{}: {e}", abs.display()))?;
    if !md.is_file() {
        return Err(format!("{} is not a file", abs.display()));
    }
    Ok(abs)
}

/// ffprobe's raw JSON for a local file. Only the `file` protocol is open to it, so a playlist cannot send it to the
/// network.
pub fn raw(l: &Located, file: &Path) -> Result<Value, String> {
    let mut cmd = Command::new(&l.ffprobe);
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .args(["-protocol_whitelist", "file"])
        .arg("-i")
        .arg(format!("file:{}", file.display()));
    let out = run(cmd, Duration::from_secs(TIMEOUT_SECS), MAX_JSON)?;
    if !out.ok {
        let why = last_line(&out.stderr);
        return Err(format!(
            "ffprobe cannot read {}: {}",
            file.display(),
            if why.is_empty() { "it failed".to_string() } else { why }
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("ffprobe's answer is not JSON: {e}"))
}

/// A number ffprobe wrote as a string or as a number.
fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `30000/1001` as 29.97; `0/0` (unknown) as None.
fn rate(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a > 0.0 && b > 0.0).then(|| round3(a / b))
}

fn text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn rotation(s: &Value) -> Option<i64> {
    s["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| num(&d["rotation"])))
        .or_else(|| num(&s["tags"]["rotate"]))
        .map(|r| r.round() as i64)
        .filter(|r| *r != 0)
}

/// The container names ffprobe lists for the file (`mov`, `mp4`, ...).
pub fn container_names(raw: &Value) -> Vec<String> {
    raw["format"]["format_name"]
        .as_str()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether the file has an audio stream.
pub fn has_audio(raw: &Value) -> bool {
    raw["streams"]
        .as_array()
        .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "audio"))
}

/// The duration in seconds the container reports, else the longest stream's.
pub fn duration_s(raw: &Value) -> Option<f64> {
    num(&raw["format"]["duration"]).or_else(|| {
        raw["streams"]
            .as_array()?
            .iter()
            .filter_map(|s| num(&s["duration"]))
            .fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.max(d))))
    })
}

/// What `media.probe` answers.
pub fn summarize(raw: &Value, file: &Path) -> Value {
    let streams = raw["streams"].as_array().cloned().unwrap_or_default();
    let (mut video, mut audio, mut subtitles, mut other) = (vec![], vec![], vec![], 0);
    for s in &streams {
        let common = |mut v: Value| {
            v["index"] = s["index"].clone();
            v["codec"] = json!(text(&s["codec_name"]));
            v["duration_s"] = json!(num(&s["duration"]).map(round3));
            v["language"] = json!(text(&s["tags"]["language"]));
            v
        };
        match s["codec_type"].as_str() {
            Some("video") => {
                // a cover picture in an audio file is a video stream of one frame
                let cover = s["disposition"]["attached_pic"] == 1;
                video.push(common(json!({
                    "width": s["width"], "height": s["height"],
                    "fps": rate(&s["avg_frame_rate"]).or_else(|| rate(&s["r_frame_rate"])),
                    "frames": num(&s["nb_frames"]).map(|n| n as u64),
                    "pix_fmt": text(&s["pix_fmt"]), "rotation": rotation(s), "cover_art": cover,
                })));
            }
            Some("audio") => audio.push(common(json!({
                "sample_rate": num(&s["sample_rate"]).map(|n| n as u64),
                "channels": s["channels"], "layout": text(&s["channel_layout"]),
            }))),
            Some("subtitle") => subtitles.push(common(json!({}))),
            _ => other += 1,
        }
    }
    let real_video = video.iter().any(|v| v["cover_art"] != true);
    let f = &raw["format"];
    json!({
        "file": file,
        "container": {"format": text(&f["format_name"]), "name": text(&f["format_long_name"]),
                      "duration_s": duration_s(raw).map(round3), "size_bytes": num(&f["size"]).map(|n| n as u64),
                      "bit_rate": num(&f["bit_rate"]).map(|n| n as u64), "streams": f["nb_streams"]},
        "has_video": real_video, "has_audio": !audio.is_empty(), "has_subtitles": !subtitles.is_empty(),
        "video": video, "audio": audio, "subtitles": subtitles, "other_streams": other,
        "tags": f["tags"],
    })
}

/// Probes a local file and summarises it. Blocking.
pub fn probe(l: &Located, path: &str) -> Result<Value, String> {
    let file = local_file(path)?;
    let raw = raw(l, &file)?;
    Ok(summarize(&raw, &file))
}

#[cfg(test)]
pub(crate) const SAMPLE: &str = r#"{
  "streams": [
    {"index": 0, "codec_name": "h264", "codec_type": "video", "width": 1080, "height": 1920, "pix_fmt": "yuv420p",
     "r_frame_rate": "60/1", "avg_frame_rate": "30000/1001", "duration": "32.000000", "nb_frames": "960",
     "tags": {"language": "und"}, "disposition": {"attached_pic": 0},
     "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]},
    {"index": 1, "codec_name": "aac", "codec_type": "audio", "sample_rate": "48000", "channels": 2,
     "channel_layout": "stereo", "duration": "32.021000", "tags": {"language": "eng"}},
    {"index": 2, "codec_name": "mov_text", "codec_type": "subtitle", "tags": {"language": "eng"}},
    {"index": 3, "codec_name": "bin_data", "codec_type": "data"}
  ],
  "format": {"filename": "x.mp4", "nb_streams": 4, "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
             "format_long_name": "QuickTime / MOV", "start_time": "0.000000", "duration": "32.021000",
             "size": "904094", "bit_rate": "225870", "tags": {"title": "meeting"}}
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ffmpeg::{locate, Origin};
    use crate::media::stub::{last_args, pair, Stub};
    use crate::testing::TestDir;

    fn sample() -> Value {
        serde_json::from_str(SAMPLE).unwrap()
    }

    #[test]
    fn the_summary_names_each_kind_of_stream() {
        let v = summarize(&sample(), Path::new("/clips/x.mp4"));
        assert_eq!(v["container"]["format"], "mov,mp4,m4a,3gp,3g2,mj2");
        assert_eq!(v["container"]["duration_s"], 32.021);
        assert_eq!(v["container"]["size_bytes"], 904094);
        assert_eq!(
            (v["has_video"].clone(), v["has_audio"].clone()),
            (json!(true), json!(true))
        );
        assert_eq!(v["has_subtitles"], true);
        let video = &v["video"][0];
        assert_eq!(
            (video["width"].clone(), video["height"].clone()),
            (json!(1080), json!(1920))
        );
        assert_eq!(video["fps"], 29.97, "the average rate wins over the container's");
        assert_eq!(video["frames"], 960);
        assert_eq!(video["rotation"], -90);
        let audio = &v["audio"][0];
        assert_eq!(
            (audio["sample_rate"].clone(), audio["channels"].clone()),
            (json!(48000), json!(2))
        );
        assert_eq!(audio["language"], "eng");
        assert_eq!(v["other_streams"], 1);
        assert_eq!(v["container"]["streams"], 4);
    }

    #[test]
    fn cover_art_is_not_a_video_and_unknown_values_are_null() {
        let raw = json!({"streams": [
            {"index": 0, "codec_type": "audio", "codec_name": "mp3", "sample_rate": "44100", "channels": 2},
            {"index": 1, "codec_type": "video", "codec_name": "mjpeg", "avg_frame_rate": "0/0", "r_frame_rate": "90000/1",
             "disposition": {"attached_pic": 1}}],
            "format": {"format_name": "mp3"}});
        let v = summarize(&raw, Path::new("a.mp3"));
        assert_eq!(v["has_video"], false, "{v}");
        assert_eq!(v["has_audio"], true);
        assert_eq!(v["video"][0]["cover_art"], true);
        assert_eq!(
            v["video"][0]["fps"], 90000.0,
            "the container's rate stands in for an unknown average"
        );
        assert!(v["container"]["duration_s"].is_null());
        // a stream that carries its own duration stands in for the container's
        let raw = json!({"streams": [{"codec_type": "audio", "duration": "3.5"}, {"codec_type": "audio", "duration": "7.25"}],
                         "format": {"format_name": "wav"}});
        assert_eq!(duration_s(&raw), Some(7.25));
        assert!(has_audio(&raw));
        assert_eq!(container_names(&sample()), ["mov", "mp4", "m4a", "3gp", "3g2", "mj2"]);
    }

    #[test]
    fn only_a_local_file_is_probed() {
        let dir = TestDir::new("mm-probe-file");
        let f = dir.join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(local_file(f.to_str().unwrap()).unwrap(), f);
        assert!(local_file("").unwrap_err().contains("required"));
        assert!(local_file(dir.to_str().unwrap()).unwrap_err().contains("not a file"));
        assert!(local_file(dir.join("none.mp4").to_str().unwrap()).is_err());
        // a URL is not a file name here
        assert!(local_file("http://example.org/a.mp4").is_err());
        assert!(local_file("concat:a.mp4|b.mp4").is_err());
    }

    #[test]
    fn ffprobe_is_asked_for_json_over_the_file_protocol_only() {
        let home = TestDir::new("mm-probe-run");
        let tools = home.join("tools");
        pair(
            &tools,
            &Stub {
                configuration: "--disable-gpl",
                probe_json: SAMPLE,
                pcm: &[],
            },
        );
        let l = locate(&home, Some(&tools)).unwrap();
        assert_eq!(l.origin, Origin::Setting);
        let clip = home.join("clip.mp4");
        std::fs::write(&clip, b"not really a video").unwrap();
        let v = probe(&l, clip.to_str().unwrap()).unwrap();
        assert_eq!(v["container"]["name"], "QuickTime / MOV");
        let args = last_args(&tools, "probe-args.log");
        for want in [
            "-print_format json",
            "-show_streams",
            "-protocol_whitelist file",
            "-i file:",
        ] {
            assert!(args.contains(want), "{want} not in {args}");
        }
        assert!(probe(&l, home.join("missing.mp4").to_str().unwrap()).is_err());
    }

    #[test]
    fn a_probe_that_fails_says_why() {
        let home = TestDir::new("mm-probe-fail");
        let tools = home.join("tools");
        pair(
            &tools,
            &Stub {
                configuration: "",
                probe_json: "not json at all",
                pcm: &[],
            },
        );
        let l = locate(&home, Some(&tools)).unwrap();
        let clip = home.join("clip.bin");
        std::fs::write(&clip, b"x").unwrap();
        assert!(probe(&l, clip.to_str().unwrap()).unwrap_err().contains("not JSON"));
    }
}

//! Any container to 16 kHz mono samples, through ffmpeg, for speech to text.
//!
//! The file is probed first and only containers that are plain audio or video files are decoded: a playlist or a
//! script (HLS, DASH, concat, image sequences, devices) can name other files and addresses, and an uploaded "audio file"
//! must not make the hub open them. ffmpeg itself gets only the `file` protocol.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::ffmpeg::Located;
use super::{last_line, probe, run, with_upload};
use crate::audio::{Request, SAMPLE_RATE};

/// The longest stretch decoded, in seconds (the samples are held in memory: 4 hours is about 900 MB).
pub const MAX_SECONDS: u64 = 4 * 3600;

/// The shortest audio that is worth transcribing, in samples (0.1 s).
const MIN_SAMPLES: usize = SAMPLE_RATE as usize / 10;

/// ffprobe's names for the containers that are accepted. The list is of plain files: the formats people record, share
/// and download. Raw streams, playlists and manifests are not in it.
const CONTAINERS: &[&str] = &[
    "mov", "mp4", "m4a", "3gp", "3g2", "matroska", "webm", "ogg", "flac", "wav", "mp3", "aac", "ac3", "eac3", "avi",
    "flv", "asf", "mpegts", "mpeg", "amr", "caf", "aiff", "w64", "wv", "ape", "tta", "mka", "opus", "au", "voc", "dts",
    "gsm", "rm", "ivf", "nut", "mxf", "swf",
];

/// Whether ffprobe's `format_name` (a comma list) names an accepted container.
pub fn accepted(format_name: &str) -> bool {
    format_name.split(',').map(str::trim).any(|n| CONTAINERS.contains(&n))
}

/// The samples of a transcription request. WAV and MP3 the hub reads itself (no ffmpeg needed); any other container, and a
/// WAV or MP3 it cannot read, goes through ffmpeg. `ffmpeg` is what `Engine::ffmpeg` said: when it is an error, a file the
/// hub cannot read itself is refused with that error after the native one. `tmp` holds an upload while ffmpeg reads it.
/// Blocking.
pub fn decode_any(ffmpeg: &Result<Located, String>, req: &Request, tmp: &Path) -> Result<Vec<f32>, String> {
    if let Some(path) = &req.path {
        let l = ffmpeg
            .as_ref()
            .map_err(|why| format!("{why} (it is needed to read {})", path.display()))?;
        return decode_file(l, path);
    }
    match crate::audio::decode(&req.file, &req.filename) {
        Ok(samples) => Ok(samples),
        Err(native) => match ffmpeg {
            Ok(l) => decode_bytes(l, &req.file, &req.filename, tmp),
            Err(why) => Err(format!(
                "{native}; or make ffmpeg available so that it reads any container: {why}"
            )),
        },
    }
}

/// The samples of the first audio stream of `bytes`, which the caller uploaded as `filename`. Blocking.
pub fn decode_bytes(l: &Located, bytes: &[u8], filename: &str, tmp: &Path) -> Result<Vec<f32>, String> {
    with_upload(tmp, bytes, filename, |path| decode_file(l, path))
}

/// The samples of the first audio stream of a local file. Blocking.
pub fn decode_file(l: &Located, file: &Path) -> Result<Vec<f32>, String> {
    let raw = probe::raw(l, file)?;
    let names = probe::container_names(&raw);
    if !accepted(&names.join(",")) {
        return Err(format!(
            "this is not an audio or video file ffmpeg is allowed to read here (its format is {}); playlists, scripts and \
             raw streams are refused",
            if names.is_empty() { "unknown".to_string() } else { names.join(",") }
        ));
    }
    if !probe::has_audio(&raw) {
        return Err("the file has no audio track".into());
    }
    let seconds = probe::duration_s(&raw).unwrap_or(0.0).clamp(0.0, MAX_SECONDS as f64);
    let mut cmd = Command::new(&l.ffmpeg);
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-protocol_whitelist",
        "file",
        "-i",
    ])
    .arg(format!("file:{}", file.display()))
    .args(["-vn", "-sn", "-dn", "-map", "0:a:0", "-t", &MAX_SECONDS.to_string()])
    .args(["-ac", "1", "-ar", &SAMPLE_RATE.to_string(), "-f", "f32le", "pipe:1"]);
    // decoding runs far faster than playing: a generous limit that still ends a stuck process
    let limit = Duration::from_secs(120 + (seconds / 10.0) as u64);
    let max = (MAX_SECONDS as usize + 1) * SAMPLE_RATE as usize * 4;
    let out = run(cmd, limit, max)?;
    if !out.ok {
        let why = last_line(&out.stderr);
        return Err(format!(
            "ffmpeg could not decode the audio: {}",
            if why.is_empty() { "it failed".into() } else { why }
        ));
    }
    let samples = samples_of(&out.stdout);
    if samples.len() < MIN_SAMPLES {
        return Err("the audio is shorter than 0.1 s".into());
    }
    Ok(samples)
}

/// Little-endian f32 samples from raw bytes (a trailing partial sample is dropped).
pub fn samples_of(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .map(|s| if s.is_finite() { s } else { 0.0 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::media::ffmpeg::locate;
    use crate::media::probe::SAMPLE;
    use crate::media::stub::{last_args, pair, Stub};
    use crate::testing::TestDir;

    fn tools(home: &Path, probe_json: &str, pcm: &[f32]) -> Located {
        let dir = home.join("tools");
        pair(
            &dir,
            &Stub {
                configuration: "--disable-gpl",
                probe_json,
                pcm,
            },
        );
        locate(home, Some(&dir)).unwrap()
    }

    #[test]
    fn plain_files_pass_and_playlists_do_not() {
        assert!(accepted("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(accepted("matroska,webm"));
        assert!(accepted("wav") && accepted("mp3") && accepted("ogg") && accepted("flac"));
        for refused in [
            "hls",
            "dash",
            "concat",
            "ffconcat",
            "image2",
            "rtsp",
            "sdp",
            "lavfi",
            "s16le",
            "",
            "applehttp",
        ] {
            assert!(!accepted(refused), "{refused}");
        }
    }

    #[test]
    fn raw_bytes_become_samples() {
        let bytes: Vec<u8> = [0.5f32, -0.25, f32::NAN, f32::INFINITY]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .chain([1u8, 2, 3])
            .collect();
        assert_eq!(samples_of(&bytes), vec![0.5, -0.25, 0.0, 0.0]);
    }

    #[test]
    fn a_video_file_gives_its_audio() {
        let home = TestDir::new("mm-decode-ok");
        let pcm = vec![0.25f32; SAMPLE_RATE as usize]; // one second
        let l = tools(&home, SAMPLE, &pcm);
        let tmp = home.join("tmp");
        let samples = decode_bytes(&l, b"pretend this is an mp4", "meeting.mp4", &tmp).unwrap();
        assert_eq!(samples.len(), pcm.len());
        assert!((samples[10] - 0.25).abs() < 1e-6);
        let args = last_args(&home.join("tools"), "args.log");
        for want in [
            "-protocol_whitelist file",
            "-vn",
            "-map 0:a:0",
            "-ac 1",
            "-ar 16000",
            "-f f32le",
            "pipe:1",
            "-t 14400",
        ] {
            assert!(args.contains(want), "{want} not in {args}");
        }
        assert!(args.contains(".mp4"), "the upload keeps its extension: {args}");
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "the uploaded copy is removed"
        );
    }

    fn request(file: &[u8], filename: &str, path: Option<PathBuf>) -> Request {
        Request {
            file: file.to_vec(),
            filename: filename.into(),
            model: String::new(),
            language: None,
            prompt: None,
            format: crate::audio::ResponseFormat::Json,
            temperature: 0.0,
            words: false,
            path,
        }
    }

    #[test]
    fn wav_is_read_by_the_hub_and_other_containers_by_ffmpeg() {
        let home = TestDir::new("mm-decode-any");
        let tmp = home.join("tmp");
        let pcm = vec![0.25f32; SAMPLE_RATE as usize];
        let found = Ok(tools(&home, SAMPLE, &pcm));
        let log = home.join("tools").join("args.log");

        // a WAV: ffmpeg is not run
        let wav: Vec<u8> = {
            let mut v = b"RIFF".to_vec();
            v.extend((36 + 32_000u32).to_le_bytes());
            v.extend(b"WAVEfmt ");
            v.extend(16u32.to_le_bytes());
            v.extend([1, 0, 1, 0]);
            v.extend(16_000u32.to_le_bytes());
            v.extend(32_000u32.to_le_bytes());
            v.extend([2, 0, 16, 0]);
            v.extend(b"data");
            v.extend(32_000u32.to_le_bytes());
            v.extend(vec![0u8; 32_000]);
            v
        };
        let s = decode_any(&found, &request(&wav, "a.wav", None), &tmp).unwrap();
        assert_eq!(s.len(), 16_000);
        assert!(!log.exists(), "a WAV needs no ffmpeg");

        // an MP4: ffmpeg is run on a copy of the upload
        let s = decode_any(&found, &request(b"not a wav", "meeting.mp4", None), &tmp).unwrap();
        assert_eq!(s.len(), pcm.len());
        assert!(log.exists());

        // a named file: ffmpeg reads it where it is, and the hub never holds its bytes
        let clip = home.join("clip.mkv");
        std::fs::write(&clip, b"x").unwrap();
        std::fs::remove_file(&log).unwrap();
        let s = decode_any(&found, &request(b"", &clip.to_string_lossy(), Some(clip.clone())), &tmp).unwrap();
        assert_eq!(s.len(), pcm.len());
        assert!(last_args(&home.join("tools"), "args.log").contains("clip.mkv"));
    }

    #[test]
    fn without_ffmpeg_a_container_the_hub_cannot_read_says_how_to_get_it() {
        let home = TestDir::new("mm-decode-noff");
        let tmp = home.join("tmp");
        let gone: Result<Located, String> = Err(crate::media::ffmpeg::NOT_INSTALLED.to_string());
        let e = decode_any(&gone, &request(b"\0\0\0\x20ftypisom", "a.mp4", None), &tmp).unwrap_err();
        assert!(
            e.contains("unsupported audio format") && e.contains("media.install"),
            "{e}"
        );
        let e = decode_any(&gone, &request(b"", "x.mp4", Some(home.join("x.mp4"))), &tmp).unwrap_err();
        assert!(e.contains("media.install") && e.contains("x.mp4"), "{e}");
    }

    #[test]
    fn a_playlist_or_a_silent_file_or_a_blip_is_refused() {
        let home = TestDir::new("mm-decode-no");
        let tmp = home.join("tmp");
        let hls =
            r#"{"streams": [{"codec_type": "audio"}], "format": {"format_name": "hls,applehttp", "duration": "10"}}"#;
        let l = tools(&home, hls, &[0.1; 16_000]);
        let e = decode_bytes(&l, b"#EXTM3U", "list.m3u8", &tmp).unwrap_err();
        assert!(e.contains("refused") && e.contains("hls"), "{e}");
        assert!(
            !home.join("tools").join("args.log").exists(),
            "ffmpeg is never run on a refused container"
        );

        let silent =
            r#"{"streams": [{"codec_type": "video"}], "format": {"format_name": "mov,mp4", "duration": "10"}}"#;
        let l = tools(&home, silent, &[0.1; 16_000]);
        assert!(decode_bytes(&l, b"x", "v.mp4", &tmp)
            .unwrap_err()
            .contains("no audio track"));

        let l = tools(&home, SAMPLE, &[0.1; 100]);
        assert!(decode_bytes(&l, b"x", "a.mp4", &tmp).unwrap_err().contains("shorter"));
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "nothing is left in the folder"
        );
    }
}

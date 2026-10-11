//! Text to speech with llama.cpp's own `llama-tts` (built from the vendored tree with the `bundled-engine` feature,
//! or found next to `llama-server`). The tool is model-agnostic: it takes a speech model's GGUF plus the model's
//! projector GGUF (`mmproj*.gguf`, which holds the audio decoder) and writes a WAV. ModelMistress pairs them the
//! way it pairs a vision model with its projector: the `mmproj*.gguf` beside the model file. A voice is a short
//! reference recording (WAV, MP3 or FLAC) the model imitates.
//!
//! Each request starts one `llama-tts` process (the model is loaded, used and freed), one at a time.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::catalog::ModelEntry;
use crate::engine::{Fail, Result};

/// The most characters one request may speak (OpenAI's limit).
pub const MAX_INPUT_CHARS: usize = 4096;

/// What OpenAI clients send as `model`: it means "the default speech model".
const OPENAI_MODELS: &[&str] = &["tts-1", "tts-1-hd", "gpt-4o-mini-tts"];

/// What OpenAI clients send as `voice`: these voices do not exist here, they mean "the default voice".
const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A WAV file, 16-bit mono.
    Wav,
    /// The raw 16-bit little-endian mono samples (OpenAI's `pcm`).
    Pcm,
}

impl Format {
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s.to_lowercase().as_str() {
            "wav" => Ok(Self::Wav),
            "pcm" => Ok(Self::Pcm),
            other => Err(format!(
                "response_format \"{other}\" is not available: only wav and pcm are produced (mp3, opus, aac and flac \
                 need an encoder that is not built in; convert the WAV, for example with ffmpeg)"
            )),
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/pcm",
        }
    }
}

/// A parsed `POST /v1/audio/speech` body (or the `speech.synthesize` operation's input).
#[derive(Debug, Clone)]
pub struct SpeechRequest {
    pub model: String,
    pub input: String,
    pub voice: String,
    pub format: Format,
    pub speed: f32,
    pub language: Option<String>,
    /// The most audio frames to generate (llama-tts `-n`); unset: the tool's own limit.
    pub max_frames: Option<u32>,
}

impl SpeechRequest {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        let obj = v.as_object().ok_or("the body must be a JSON object")?;
        let text = |k: &str| -> std::result::Result<Option<String>, String> {
            match obj.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
                Some(_) => Err(format!("{k} must be a string")),
            }
        };
        let input = text("input")?.filter(|s| !s.is_empty()).ok_or("input is required")?;
        if input.chars().count() > MAX_INPUT_CHARS {
            return Err(format!("input is over {MAX_INPUT_CHARS} characters"));
        }
        // OpenAI also allows {"voice": {"id": "..."}}
        let voice = match obj.get("voice") {
            Some(Value::Object(o)) => o.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            _ => text("voice")?.unwrap_or_default(),
        };
        let speed = match obj.get("speed") {
            None | Some(Value::Null) => 1.0,
            Some(x) => x.as_f64().ok_or("speed must be a number")? as f32,
        };
        if !(0.25..=4.0).contains(&speed) {
            return Err("speed must be between 0.25 and 4.0".into());
        }
        let max_frames = match obj.get("max_frames") {
            None | Some(Value::Null) => None,
            Some(x) => Some(
                x.as_u64()
                    .filter(|n| (1..=100_000).contains(n))
                    .ok_or("max_frames must be an integer from 1 to 100000")? as u32,
            ),
        };
        Ok(Self {
            model: text("model")?.unwrap_or_default(),
            input,
            voice,
            format: Format::parse(&text("response_format")?.unwrap_or_else(|| "wav".into()))?,
            speed,
            language: text("language")?.filter(|l| !l.is_empty()).map(|l| l.to_lowercase()),
            max_frames,
        })
    }
}

/// Is this the name an OpenAI client sends for "whatever speech model there is"?
pub fn is_generic_model(name: &str) -> bool {
    let n = name.trim();
    n.is_empty() || OPENAI_MODELS.iter().any(|m| m.eq_ignore_ascii_case(n))
}

/// Does this catalog entry look like a speech model: it has a projector and its name or architecture says TTS.
fn looks_like_tts(e: &ModelEntry) -> bool {
    e.mmproj.is_some()
        && (e.id.to_lowercase().contains("tts") || e.architecture().is_some_and(|a| a.to_lowercase().contains("tts")))
}

/// The speech model to use when the request names none: the `tts_model` setting, else the only one found.
pub fn pick_default(entries: &[ModelEntry], preferred: Option<&str>) -> Result<ModelEntry> {
    if let Some(p) = preferred.map(str::trim).filter(|p| !p.is_empty()) {
        return entries
            .iter()
            .find(|e| e.id.eq_ignore_ascii_case(p))
            .cloned()
            .ok_or_else(|| {
                Fail::not_found(format!(
                    "the tts_model setting names \"{p}\", which is not on this machine"
                ))
            });
    }
    let found: Vec<&ModelEntry> = entries.iter().filter(|e| looks_like_tts(e)).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(Fail::not_found(
            "no text-to-speech model on this machine: download a speech model with its projector (mmproj*.gguf beside \
             it), for example `model-mistress pull hf.co/ggml-org/Qwen3-TTS-12Hz-1.7B-Base-GGUF`; see docs/tools.md",
        )),
        many => Err(Fail::invalid(format!(
            "several speech models are here ({}); name one in the request or set tts_model",
            many.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// The model file and its projector, or a clear error.
pub fn files_of(e: &ModelEntry) -> Result<(PathBuf, PathBuf)> {
    match &e.mmproj {
        Some(p) => Ok((e.path.clone(), p.clone())),
        None => Err(Fail::invalid(format!(
            "{} has no projector: a speech model needs its mmproj*.gguf in the same folder (llama-tts takes the model \
             and the projector together)",
            e.id
        ))),
    }
}

const VOICE_EXTENSIONS: &[&str] = &["wav", "mp3", "flac"];

/// The reference recording for `voice`. "" and the OpenAI voice names mean `default` (the `tts_voice` setting), and
/// without one, no recording (some models speak in a default voice, others need one). A voice is the name of a
/// recording in `voices_dir` (`<name>.wav`, `.mp3` or `.flac`) or the full path of one.
pub fn find_voice(voices_dir: &Path, voice: &str, default: Option<&str>) -> Result<Option<PathBuf>> {
    let voice = voice.trim();
    let generic = voice.is_empty() || OPENAI_VOICES.iter().any(|v| v.eq_ignore_ascii_case(voice));
    if generic {
        return match default.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => find_voice(voices_dir, d, None),
            None => Ok(None),
        };
    }
    let as_path = Path::new(voice);
    if as_path.is_absolute() {
        return if as_path.is_file() {
            Ok(Some(as_path.to_path_buf()))
        } else {
            Err(Fail::not_found(format!("no voice file {voice}")))
        };
    }
    if voice.contains(['/', '\\', ':']) || voice.contains("..") {
        return Err(Fail::invalid(
            "a voice is a file name in the voices folder, or the full path of a recording",
        ));
    }
    for ext in VOICE_EXTENSIONS {
        let p = voices_dir.join(format!("{voice}.{ext}"));
        if p.is_file() {
            return Ok(Some(p));
        }
    }
    Err(Fail::not_found(format!(
        "no voice \"{voice}\": put a short recording of it at {}",
        voices_dir.join(format!("{voice}.wav")).display()
    )))
}

/// The arguments of `llama-tts` for one request.
pub fn tool_args(
    model: &Path,
    mmproj: &Path,
    req: &SpeechRequest,
    speaker: Option<&Path>,
    out: &Path,
    gpu_layers: i32,
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-m".into(),
        model.display().to_string(),
        "--mmproj".into(),
        mmproj.display().to_string(),
        "-p".into(),
        req.input.clone(),
        "-o".into(),
        out.display().to_string(),
        "-ngl".into(),
        gpu_layers.to_string(),
    ];
    if let Some(s) = speaker {
        a.extend(["--tts-speaker-file".into(), s.display().to_string()]);
    }
    if let Some(l) = &req.language {
        a.extend(["--tts-lang".into(), l.clone()]);
    }
    if let Some(n) = req.max_frames {
        a.extend(["-n".into(), n.to_string()]);
    }
    a
}

// ---- WAV ----------------------------------------------------------------------------------------------------

/// 16-bit mono PCM samples and their sample rate.
#[derive(Debug)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<i16>,
}

/// Reads a WAV file of 16-bit PCM (what llama-tts writes); several channels are averaged into one.
pub fn parse_wav(b: &[u8]) -> std::result::Result<Pcm, String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut rate, mut channels, mut bits, mut format) = (0u32, 0usize, 0u16, 0u16);
    let mut pos = 12;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(b.len());
        if id == b"fmt " && end - body >= 16 {
            format = u16::from_le_bytes([b[body], b[body + 1]]);
            channels = u16::from_le_bytes([b[body + 2], b[body + 3]]) as usize;
            rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
            bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
        } else if id == b"data" {
            if format != 1 || bits != 16 || channels == 0 || rate == 0 {
                return Err(format!(
                    "the WAV is not 16-bit PCM (format {format}, {bits} bits, {channels} channels)"
                ));
            }
            let samples = b[body..end]
                .chunks_exact(2 * channels)
                .map(|f| {
                    let sum: i32 = (0..channels)
                        .map(|c| i16::from_le_bytes([f[2 * c], f[2 * c + 1]]) as i32)
                        .sum();
                    (sum / channels as i32) as i16
                })
                .collect();
            return Ok(Pcm { rate, samples });
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Err("the WAV has no audio data".into())
}

pub fn wav_bytes(p: &Pcm) -> Vec<u8> {
    let data = p.samples.len() as u32 * 2;
    let mut v = Vec::with_capacity(44 + data as usize);
    v.extend(b"RIFF");
    v.extend((36 + data).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend([1, 0, 1, 0]); // PCM, mono
    v.extend(p.rate.to_le_bytes());
    v.extend((p.rate * 2).to_le_bytes());
    v.extend([2, 0, 16, 0]);
    v.extend(b"data");
    v.extend(data.to_le_bytes());
    p.samples.iter().for_each(|s| v.extend(s.to_le_bytes()));
    v
}

pub fn pcm_bytes(p: &Pcm) -> Vec<u8> {
    p.samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Plays the samples faster or slower without changing the pitch (WSOLA: overlapping 25 ms windows, each placed
/// where it best continues the one before). `speed` 2.0 makes the audio half as long.
pub fn change_speed(x: &[i16], speed: f32, rate: u32) -> Vec<i16> {
    let n = ((rate as usize / 40).max(64)) & !1;
    if (speed - 1.0).abs() < 0.01 || x.len() < 4 * n {
        return x.to_vec();
    }
    let hs = n / 2; // synthesis hop
    let tol = hs / 2; // how far from the nominal position a window may move
    let win: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect();
    let xf: Vec<f32> = x.iter().map(|&s| s as f32).collect();
    let out_len = (x.len() as f32 / speed) as usize;
    let mut out = vec![0f32; out_len + 2 * n];
    let mut norm = vec![0f32; out_len + 2 * n];
    let mut prev = 0usize;
    let mut k = 0usize;
    loop {
        let out_pos = k * hs;
        let nominal = (k as f32 * hs as f32 * speed) as usize;
        if out_pos >= out_len || nominal + n > xf.len() {
            break;
        }
        let pos = if k == 0 {
            0
        } else {
            // the window that would naturally follow the previous one is the template
            let t = prev + hs;
            let lo = nominal.saturating_sub(tol);
            let hi = (nominal + tol).min(xf.len() - n);
            if t + n > xf.len() || lo > hi {
                nominal
            } else {
                let mut best = (f32::MIN, nominal);
                for c in lo..=hi {
                    let score: f32 = (0..n).step_by(2).map(|i| xf[c + i] * xf[t + i]).sum();
                    if score > best.0 {
                        best = (score, c);
                    }
                }
                best.1
            }
        };
        for i in 0..n {
            out[out_pos + i] += win[i] * xf[pos + i];
            norm[out_pos + i] += win[i];
        }
        prev = pos;
        k += 1;
    }
    out.iter()
        .zip(&norm)
        .take(out_len)
        .map(|(&o, &w)| (if w > 1e-3 { o / w } else { 0.0 }).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// The WAV llama-tts wrote, turned into the answer: speed applied, in the format asked for.
pub fn finish(wav: &[u8], req: &SpeechRequest) -> std::result::Result<(Vec<u8>, f64), String> {
    let mut pcm = parse_wav(wav)?;
    if (req.speed - 1.0).abs() >= 0.01 {
        pcm.samples = change_speed(&pcm.samples, req.speed, pcm.rate);
    }
    let seconds = pcm.samples.len() as f64 / pcm.rate as f64;
    let bytes = match req.format {
        Format::Wav => wav_bytes(&pcm),
        Format::Pcm => pcm_bytes(&pcm),
    };
    Ok((bytes, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, mmproj: bool) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            source: "folder",
            path: PathBuf::from("/m").join(format!("{id}.gguf")),
            size_bytes: 1,
            mmproj: mmproj.then(|| PathBuf::from("/m/mmproj-x.gguf")),
            meta: Default::default(),
            native: None,
        }
    }

    fn sine(rate: u32, seconds: f32, hz: f32) -> Vec<i16> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| ((i as f32 * hz * 2.0 * std::f32::consts::PI / rate as f32).sin() * 12000.0) as i16)
            .collect()
    }

    fn crossings(x: &[i16]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count()
    }

    #[test]
    fn parsing_requests() {
        let r = SpeechRequest::from_json(&json!({"model": "tts-1", "input": " Hello ", "voice": "alloy"})).unwrap();
        assert_eq!((r.input.as_str(), r.format, r.speed), ("Hello", Format::Wav, 1.0));
        let r = SpeechRequest::from_json(
            &json!({"input": "x", "voice": {"id": "anna"}, "response_format": "PCM", "speed": 1.5, "language": "DE", "max_frames": 300}),
        )
        .unwrap();
        assert_eq!(
            (r.voice.as_str(), r.format, r.speed, r.language.as_deref(), r.max_frames),
            ("anna", Format::Pcm, 1.5, Some("de"), Some(300))
        );
        let bad = |v: Value| SpeechRequest::from_json(&v).unwrap_err();
        assert!(bad(json!({})).contains("input"));
        assert!(bad(json!({"input": "  "})).contains("input"));
        assert!(bad(json!({"input": 5})).contains("string"));
        assert!(bad(json!([1])).contains("object"));
        assert!(bad(json!({"input": "x".repeat(MAX_INPUT_CHARS + 1)})).contains("4096"));
        assert!(bad(json!({"input": "x", "speed": 9})).contains("speed"));
        assert!(bad(json!({"input": "x", "speed": "fast"})).contains("speed"));
        assert!(bad(json!({"input": "x", "max_frames": 0})).contains("max_frames"));
        let e = bad(json!({"input": "x", "response_format": "mp3"}));
        assert!(e.contains("mp3") && e.contains("wav and pcm"), "{e}");
    }

    #[test]
    fn choosing_the_model() {
        let tts = entry("Qwen3-TTS-1.7B", true);
        let chat = entry("llama3.2:1b", false);
        let vision = entry("gemma-vision", true);
        let all = [chat.clone(), vision.clone(), tts.clone()];
        assert_eq!(pick_default(&all, None).unwrap().id, tts.id);
        assert_eq!(pick_default(&all, Some("gemma-vision")).unwrap().id, "gemma-vision");
        assert_eq!(pick_default(&all, Some("nope")).unwrap_err().status, 404);
        let none = pick_default(&[chat.clone(), vision], None).unwrap_err();
        assert!(none.status == 404 && none.message.contains("no text-to-speech model"));
        let two = pick_default(&[tts.clone(), entry("pocket-tts", true)], None).unwrap_err();
        assert!(two.status == 400 && two.message.contains("pocket-tts"));
        assert!(is_generic_model("") && is_generic_model("TTS-1") && !is_generic_model("pocket-tts"));
        assert!(files_of(&tts).is_ok());
        assert!(files_of(&chat).unwrap_err().message.contains("projector"));
    }

    #[test]
    fn finding_voices() {
        let d = std::env::temp_dir().join(format!("mm-voices-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("anna.mp3"), b"x").unwrap();
        std::fs::write(d.join("bob.wav"), b"x").unwrap();
        assert_eq!(find_voice(&d, "anna", None).unwrap(), Some(d.join("anna.mp3")));
        assert_eq!(find_voice(&d, "bob", None).unwrap(), Some(d.join("bob.wav")));
        assert_eq!(find_voice(&d, "", None).unwrap(), None);
        assert_eq!(find_voice(&d, "alloy", None).unwrap(), None);
        assert_eq!(find_voice(&d, "Nova", Some("bob")).unwrap(), Some(d.join("bob.wav")));
        assert_eq!(find_voice(&d, "", Some("anna")).unwrap(), Some(d.join("anna.mp3")));
        assert_eq!(
            find_voice(&d, &d.join("bob.wav").display().to_string(), None).unwrap(),
            Some(d.join("bob.wav"))
        );
        assert_eq!(find_voice(&d, "carl", None).unwrap_err().status, 404);
        assert_eq!(find_voice(&d, "../anna", None).unwrap_err().status, 400);
        assert_eq!(find_voice(&d, "sub/anna", None).unwrap_err().status, 400);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn building_the_arguments() {
        let mut req = SpeechRequest::from_json(&json!({"input": "Hi there"})).unwrap();
        let a = tool_args(
            Path::new("m.gguf"),
            Path::new("p.gguf"),
            &req,
            None,
            Path::new("o.wav"),
            99,
        );
        assert_eq!(
            a,
            ["-m", "m.gguf", "--mmproj", "p.gguf", "-p", "Hi there", "-o", "o.wav", "-ngl", "99"]
        );
        req.language = Some("de".into());
        req.max_frames = Some(200);
        let a = tool_args(
            Path::new("m"),
            Path::new("p"),
            &req,
            Some(Path::new("v.wav")),
            Path::new("o"),
            0,
        );
        assert_eq!(
            &a[10..],
            ["--tts-speaker-file", "v.wav", "--tts-lang", "de", "-n", "200"]
        );
    }

    #[test]
    fn wav_round_trip_and_pcm() {
        let p = Pcm {
            rate: 24_000,
            samples: vec![0, 1000, -1000, 32767, -32768],
        };
        let w = wav_bytes(&p);
        assert_eq!(w.len(), 44 + 10);
        let back = parse_wav(&w).unwrap();
        assert_eq!((back.rate, back.samples.as_slice()), (24_000, p.samples.as_slice()));
        assert_eq!(pcm_bytes(&p).len(), 10);
        assert!(parse_wav(b"nope").is_err());
        let mut float = w.clone();
        float[20] = 3; // IEEE float format
        assert!(parse_wav(&float).unwrap_err().contains("16-bit PCM"));
        // a stereo file is averaged to mono
        let mut st = wav_bytes(&Pcm {
            rate: 8000,
            samples: vec![100, 300],
        });
        st[22] = 2;
        assert_eq!(parse_wav(&st).unwrap().samples, vec![200]);
    }

    #[test]
    fn speed_changes_length_and_keeps_pitch() {
        let x = sine(24_000, 2.0, 220.0);
        let base = crossings(&x) as f32 / 2.0; // per second
        for speed in [2.0f32, 0.5, 1.25] {
            let y = change_speed(&x, speed, 24_000);
            let want = x.len() as f32 / speed;
            assert!(
                (y.len() as f32 - want).abs() < 0.05 * want,
                "speed {speed}: {} vs {want}",
                y.len()
            );
            let per_second = crossings(&y) as f32 / (y.len() as f32 / 24_000.0);
            assert!(
                (per_second - base).abs() < 0.1 * base,
                "speed {speed}: {per_second} vs {base}"
            );
        }
        assert_eq!(change_speed(&x, 1.0, 24_000), x);
        assert_eq!(change_speed(&x[..100], 2.0, 24_000).len(), 100); // too short to stretch
    }

    #[test]
    fn finishing_applies_speed_and_format() {
        let wav = wav_bytes(&Pcm {
            rate: 24_000,
            samples: sine(24_000, 1.0, 300.0),
        });
        let mut req = SpeechRequest::from_json(&json!({"input": "x"})).unwrap();
        let (b, secs) = finish(&wav, &req).unwrap();
        assert!(b.starts_with(b"RIFF") && (secs - 1.0).abs() < 0.001);
        req.format = Format::Pcm;
        req.speed = 2.0;
        let (b, secs) = finish(&wav, &req).unwrap();
        assert!(!b.starts_with(b"RIFF") && (secs - 0.5).abs() < 0.05, "{secs}");
        assert!(finish(b"junk", &req).is_err());
    }
}

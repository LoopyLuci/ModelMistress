//! Speech to text, the parts that need no model: reading the multipart upload of OpenAI's
//! `/v1/audio/transcriptions` and `/v1/audio/translations`, turning the uploaded audio (WAV, MP3) into the 16 kHz
//! mono samples whisper.cpp wants, and formatting the transcript as json, text, verbose_json, srt or vtt.
//! The model itself is in `speech` (feature `whisper`).

use serde_json::{json, Value};

/// The sample rate whisper models are trained on.
pub const SAMPLE_RATE: u32 = 16_000;

// ---- the request ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Json,
    Text,
    VerboseJson,
    Srt,
    Vtt,
}

impl ResponseFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s.trim().to_lowercase().as_str() {
            "json" => Self::Json,
            "text" => Self::Text,
            "verbose_json" => Self::VerboseJson,
            "srt" => Self::Srt,
            "vtt" => Self::Vtt,
            other => {
                return Err(format!(
                    "response_format \"{other}\" is not supported (json, text, verbose_json, srt, vtt)"
                ))
            }
        })
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json | Self::VerboseJson => "application/json",
            _ => "text/plain; charset=utf-8",
        }
    }
}

/// One `/v1/audio/transcriptions` (or `translations`) request, parsed.
#[derive(Debug)]
pub struct Request {
    pub file: Vec<u8>,
    pub filename: String,
    /// "" when the request named none (then the only, or the best, whisper model on the machine is used).
    pub model: String,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub format: ResponseFormat,
    pub temperature: f32,
}

struct Part {
    name: String,
    filename: Option<String>,
    data: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let off = hay[i..=hay.len() - needle.len()].iter().position(|&b| b == first)?;
        i += off;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn boundary(content_type: &str) -> Result<String, String> {
    let mut it = content_type.split(';');
    if !it
        .next()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("multipart/form-data"))
    {
        return Err("send the audio as multipart/form-data (fields: file, model, ...)".into());
    }
    for p in it {
        if let Some((k, v)) = p.split_once('=') {
            if k.trim().eq_ignore_ascii_case("boundary") {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    return Ok(v.to_string());
                }
            }
        }
    }
    Err("the multipart Content-Type has no boundary".into())
}

/// The value of `key="..."` in a Content-Disposition header.
fn disposition_param(header: &str, key: &str) -> Option<String> {
    let lower = header.to_lowercase();
    let pat = format!("{key}=\"");
    let mut at = 0;
    while let Some(i) = lower[at..].find(&pat) {
        let start = at + i;
        // `name=` must not match inside `filename=`
        if start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric() {
            let rest = &header[start + pat.len()..];
            return rest.find('"').map(|e| rest[..e].to_string());
        }
        at = start + pat.len();
    }
    None
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Result<Vec<Part>, String> {
    let delim = format!("--{}", boundary(content_type)?).into_bytes();
    let mut parts = Vec::new();
    let mut pos = find(body, &delim, 0).ok_or("the multipart body has no parts")? + delim.len();
    loop {
        // After a delimiter: "--" ends the body, otherwise CRLF and the part's headers.
        if body[pos..].starts_with(b"--") {
            break;
        }
        if body[pos..].starts_with(b"\r\n") {
            pos += 2;
        } else {
            return Err("the multipart body is malformed".into());
        }
        let head_end = find(body, b"\r\n\r\n", pos).ok_or("the multipart body is cut short")?;
        let head = String::from_utf8_lossy(&body[pos..head_end]).to_string();
        let data_start = head_end + 4;
        let mut next = delim.clone();
        next.splice(0..0, *b"\r\n");
        let data_end = find(body, &next, data_start).ok_or("the multipart body is cut short (no closing boundary)")?;
        let disposition = head
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-disposition"))
            .unwrap_or("");
        if let Some(name) = disposition_param(disposition, "name") {
            parts.push(Part {
                name,
                filename: disposition_param(disposition, "filename"),
                data: body[data_start..data_end].to_vec(),
            });
        }
        pos = data_end + next.len();
        if pos >= body.len() {
            break;
        }
    }
    Ok(parts)
}

/// Read the fields OpenAI defines: file, model, language, prompt, response_format, temperature
/// (timestamp_granularities[] and the like are accepted and ignored).
pub fn parse_request(content_type: &str, body: &[u8]) -> Result<Request, String> {
    let parts = parse_multipart(content_type, body)?;
    let text = |name: &str| {
        parts
            .iter()
            .find(|p| p.name == name && p.filename.is_none())
            .map(|p| String::from_utf8_lossy(&p.data).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let file = parts
        .iter()
        .find(|p| p.name == "file")
        .ok_or("the \"file\" field (the audio) is missing")?;
    if file.data.is_empty() {
        return Err("the \"file\" field is empty".into());
    }
    let temperature = match text("temperature") {
        Some(t) => {
            let v: f32 = t.parse().map_err(|_| format!("temperature \"{t}\" is not a number"))?;
            if !(0.0..=1.0).contains(&v) {
                return Err("temperature must be between 0 and 1".into());
            }
            v
        }
        None => 0.0,
    };
    Ok(Request {
        file: file.data.clone(),
        filename: file.filename.clone().unwrap_or_default(),
        model: text("model").unwrap_or_default(),
        language: text("language").map(|l| l.to_lowercase()),
        prompt: text("prompt"),
        format: match text("response_format") {
            Some(f) => ResponseFormat::parse(&f)?,
            None => ResponseFormat::Json,
        },
        temperature,
    })
}

// ---- decoding ------------------------------------------------------------------------------------------------

/// Uploaded audio to 16 kHz mono f32 samples. WAV (PCM 8/16/24/32-bit, float 32/64-bit, any rate and channel
/// count) is read directly; MP3 needs the `whisper` feature (it brings the decoder). Everything else is a clear error.
pub fn decode(bytes: &[u8], filename: &str) -> Result<Vec<f32>, String> {
    let (mono, rate) = if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        decode_wav(bytes)?
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() > 2 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
        || filename.to_lowercase().ends_with(".mp3")
    {
        decode_mp3(bytes)?
    } else {
        let kind = if bytes.starts_with(b"OggS") {
            "Ogg"
        } else if bytes.starts_with(b"fLaC") {
            "FLAC"
        } else if bytes.len() > 8 && &bytes[4..8] == b"ftyp" {
            "MP4/M4A"
        } else if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
            "WebM/Matroska"
        } else {
            "unrecognised"
        };
        return Err(format!(
            "unsupported audio format ({kind}); send WAV or MP3, or convert it first (for example with ffmpeg: \
ffmpeg -i in.{} -ar 16000 -ac 1 out.wav)",
            filename.rsplit('.').next().filter(|e| e.len() <= 5).unwrap_or("m4a")
        ));
    };
    let out = resample(&mono, rate);
    if out.len() < SAMPLE_RATE as usize / 10 {
        return Err("the audio is shorter than 0.1 s".into());
    }
    Ok(out)
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// RIFF/WAVE to (mono samples, sample rate).
pub fn decode_wav(b: &[u8]) -> Result<(Vec<f32>, u32), String> {
    let bad = |m: &str| format!("not a usable WAV file: {m}");
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = le32(b, pos + 4) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(b.len()); // streamed files claim 0xFFFFFFFF
        if id == b"fmt " {
            if end - body < 16 {
                return Err(bad("the fmt chunk is too short"));
            }
            let mut tag = le16(b, body);
            if tag == 0xFFFE && end - body >= 26 {
                tag = le16(b, body + 24); // WAVE_FORMAT_EXTENSIBLE: the real tag opens the sub-format GUID
            }
            fmt = Some((tag, le16(b, body + 2), le32(b, body + 4), le16(b, body + 14)));
        } else if id == b"data" {
            data = Some(&b[body..end]);
            break;
        }
        pos = end + (size & 1);
    }
    let (tag, channels, rate, bits) = fmt.ok_or_else(|| bad("no fmt chunk"))?;
    let data = data.ok_or_else(|| bad("no data chunk"))?;
    if channels == 0 || rate == 0 {
        return Err(bad("zero channels or sample rate"));
    }
    let width = (bits as usize).div_ceil(8);
    let sample = |s: &[u8]| -> Result<f32, String> {
        Ok(match (tag, bits) {
            (1, 8) => (s[0] as f32 - 128.0) / 128.0,
            (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            (3, 64) => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
            _ => {
                return Err(format!(
                    "WAV encoding {tag} with {bits} bits is not supported (use PCM 8/16/24/32-bit or float)"
                ))
            }
        })
    };
    if width == 0 {
        return Err(bad("zero bits per sample"));
    }
    let frame = width * channels as usize;
    let mut mono = Vec::with_capacity(data.len() / frame);
    for f in data.chunks_exact(frame) {
        let mut sum = 0.0;
        for c in 0..channels as usize {
            sum += sample(&f[c * width..])?;
        }
        mono.push(sum / channels as f32);
    }
    Ok((mono, rate))
}

#[cfg(feature = "whisper")]
fn decode_mp3(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let bad = |e: Error| format!("not a usable MP3 file: {e}");
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(bad)?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("the MP3 file has no audio")?;
    let id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(bad)?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) | Err(Error::ResetRequired) => break,
            Err(e) => return Err(bad(e)),
        };
        if packet.track_id() != id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                rate = spec.rate;
                let channels = spec.channels.count().max(1);
                let mut s = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                s.copy_interleaved_ref(buf);
                mono.extend(
                    s.samples()
                        .chunks_exact(channels)
                        .map(|f| f.iter().sum::<f32>() / channels as f32),
                );
            }
            Err(Error::DecodeError(_)) => continue, // a damaged frame; skip it
            Err(e) => return Err(bad(e)),
        }
    }
    if rate == 0 || mono.is_empty() {
        return Err("the MP3 file holds no audio".into());
    }
    Ok((mono, rate))
}

#[cfg(not(feature = "whisper"))]
fn decode_mp3(_: &[u8]) -> Result<(Vec<f32>, u32), String> {
    Err("MP3 needs the `whisper` feature".into())
}

/// Mono samples at `rate` to 16 kHz: area averaging when going down (a cheap low-pass), linear interpolation up.
pub fn resample(mono: &[f32], rate: u32) -> Vec<f32> {
    if rate == SAMPLE_RATE || mono.is_empty() {
        return mono.to_vec();
    }
    let ratio = rate as f64 / SAMPLE_RATE as f64;
    let n = (mono.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(n);
    for j in 0..n {
        let p = j as f64 * ratio;
        if ratio > 1.0 {
            let (a, b) = (p, p + ratio);
            let (i0, i1) = (a.floor() as usize, (b.ceil() as usize).min(mono.len()));
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for (i, &s) in mono.iter().enumerate().take(i1).skip(i0) {
                let w = (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
                sum += s as f64 * w;
                weight += w;
            }
            out.push(if weight > 0.0 { (sum / weight) as f32 } else { 0.0 });
        } else {
            let i = p.floor() as usize;
            let frac = (p - i as f64) as f32;
            let next = mono.get(i + 1).copied().unwrap_or(mono[i]);
            out.push(mono[i] * (1.0 - frac) + next * frac);
        }
    }
    out
}

// ---- the transcript ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub no_speech_prob: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// "transcribe" or "translate".
    pub task: &'static str,
    /// The language's English name ("english"), as OpenAI's verbose_json gives it.
    pub language: String,
    pub duration: f64,
    pub segments: Vec<Segment>,
    pub temperature: f32,
}

impl Transcript {
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The response body for `format` (its content type is `ResponseFormat::content_type`).
    pub fn render(&self, format: ResponseFormat) -> String {
        match format {
            ResponseFormat::Json => json!({"text": self.text()}).to_string(),
            ResponseFormat::Text => format!("{}\n", self.text()),
            ResponseFormat::VerboseJson => self.verbose().to_string(),
            ResponseFormat::Srt => self
                .segments
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    format!(
                        "{}\n{} --> {}\n{}\n\n",
                        i + 1,
                        timestamp(s.start, ','),
                        timestamp(s.end, ','),
                        s.text.trim()
                    )
                })
                .collect(),
            ResponseFormat::Vtt => {
                let mut out = String::from("WEBVTT\n\n");
                for s in &self.segments {
                    out += &format!(
                        "{} --> {}\n{}\n\n",
                        timestamp(s.start, '.'),
                        timestamp(s.end, '.'),
                        s.text.trim()
                    );
                }
                out
            }
        }
    }

    pub fn verbose(&self) -> Value {
        json!({
            "task": self.task,
            "language": self.language,
            "duration": round3(self.duration),
            "text": self.text(),
            "segments": self.segments.iter().enumerate().map(|(i, s)| json!({
                "id": i, "seek": 0, "start": round3(s.start), "end": round3(s.end), "text": s.text,
                "tokens": [], "temperature": self.temperature, "no_speech_prob": s.no_speech_prob,
            })).collect::<Vec<_>>(),
        })
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `HH:MM:SS,mmm` (srt) or `HH:MM:SS.mmm` (vtt).
pub fn timestamp(seconds: f64, sep: char) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multipart(fields: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
        let mut body = Vec::new();
        for (name, file, data) in fields {
            body.extend(b"--XBOUND\r\n");
            match file {
                Some(f) => body.extend(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: audio/wav\r\n\r\n")
                        .as_bytes(),
                ),
                None => body.extend(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
            }
            body.extend(*data);
            body.extend(b"\r\n");
        }
        body.extend(b"--XBOUND--\r\n");
        ("multipart/form-data; boundary=XBOUND".into(), body)
    }

    pub fn wav16(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend((36 + samples.len() as u32 * 2).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * channels as u32 * 2).to_le_bytes());
        v.extend((channels * 2).to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend((samples.len() as u32 * 2).to_le_bytes());
        for s in samples {
            v.extend(s.to_le_bytes());
        }
        v
    }

    #[test]
    fn parses_all_fields() {
        let (ct, body) = multipart(&[
            ("file", Some("a.wav"), b"RIFFdata\r\nwith crlf"),
            ("model", None, b"whisper-1"),
            ("language", None, b"DE"),
            ("prompt", None, b"names: Anna"),
            ("response_format", None, b"verbose_json"),
            ("temperature", None, b"0.4"),
            ("timestamp_granularities[]", None, b"word"),
        ]);
        let r = parse_request(&ct, &body).unwrap();
        assert_eq!(r.file, b"RIFFdata\r\nwith crlf");
        assert_eq!(r.filename, "a.wav");
        assert_eq!(r.model, "whisper-1");
        assert_eq!(r.language.as_deref(), Some("de"));
        assert_eq!(r.prompt.as_deref(), Some("names: Anna"));
        assert_eq!(r.format, ResponseFormat::VerboseJson);
        assert!((r.temperature - 0.4).abs() < 1e-6);
    }

    #[test]
    fn defaults_and_quoted_boundary() {
        let (_, body) = multipart(&[("file", Some("a.wav"), b"x")]);
        let r = parse_request("multipart/form-data; boundary=\"XBOUND\"", &body).unwrap();
        assert_eq!(r.format, ResponseFormat::Json);
        assert_eq!(r.model, "");
        assert_eq!(r.temperature, 0.0);
    }

    #[test]
    fn rejects_bad_requests() {
        let (ct, body) = multipart(&[("model", None, b"m")]);
        assert!(parse_request(&ct, &body).unwrap_err().contains("\"file\""));
        let (ct, body) = multipart(&[("file", Some("a.wav"), b"x"), ("response_format", None, b"mp3")]);
        assert!(parse_request(&ct, &body).unwrap_err().contains("response_format"));
        let (ct, body) = multipart(&[("file", Some("a.wav"), b"x"), ("temperature", None, b"2")]);
        assert!(parse_request(&ct, &body).unwrap_err().contains("temperature"));
        let (ct, body) = multipart(&[("file", Some("a.wav"), b"")]);
        assert!(parse_request(&ct, &body).unwrap_err().contains("empty"));
        assert!(parse_request("application/json", b"{}")
            .unwrap_err()
            .contains("multipart"));
        assert!(parse_request("multipart/form-data", b"")
            .unwrap_err()
            .contains("boundary"));
        assert!(parse_request("multipart/form-data; boundary=B", b"junk").is_err());
        // cut off before the closing boundary
        let (ct, body) = multipart(&[("file", Some("a.wav"), b"x")]);
        assert!(parse_request(&ct, &body[..body.len() - 20]).is_err());
    }

    #[test]
    fn wav_mono_and_stereo_are_resampled_to_16k_mono() {
        let mono: Vec<i16> = (0..32_000).map(|i| ((i % 100) as i16 - 50) * 100).collect();
        let out = decode(&wav16(32_000, 1, &mono), "a.wav").unwrap();
        assert_eq!(out.len(), 16_000);
        let stereo: Vec<i16> = (0..16_000).flat_map(|_| [1000i16, -1000]).collect();
        let out = decode(&wav16(16_000, 2, &stereo), "a.wav").unwrap();
        assert_eq!(out.len(), 16_000);
        assert!(
            out.iter().all(|s| s.abs() < 1e-6),
            "opposite channels average to silence"
        );
        let up = decode(&wav16(8_000, 1, &[8192; 8_000]), "a.wav").unwrap();
        assert_eq!(up.len(), 16_000);
        assert!((up[100] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn wav_other_encodings() {
        // 24-bit extensible header
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend(40u32.to_le_bytes());
        v.extend(0xFFFEu16.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(16_000u32.to_le_bytes());
        v.extend(48_000u32.to_le_bytes());
        v.extend(3u16.to_le_bytes());
        v.extend(24u16.to_le_bytes());
        v.extend(22u16.to_le_bytes());
        v.extend(24u16.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend(1u16.to_le_bytes()); // sub-format GUID starts with the PCM tag
        v.extend([0u8; 14]);
        v.extend(b"data");
        v.extend(0xFFFF_FFFFu32.to_le_bytes()); // streamed: size unknown
        for _ in 0..3200 {
            v.extend([0x00, 0x00, 0x40]); // 0.5
        }
        let (s, rate) = decode_wav(&v).unwrap();
        assert_eq!((s.len(), rate), (3200, 16_000));
        assert!((s[0] - 0.5).abs() < 1e-4);
        // 8-bit unsigned
        let mut w = wav16(16_000, 1, &[]);
        w[34] = 8; // bits per sample
        w.extend([128u8, 255, 0]);
        let n = w.len();
        w[n - 3 - 4..n - 3].copy_from_slice(&3u32.to_le_bytes());
        let (s, _) = decode_wav(&w).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], 0.0);
        assert!(s[1] > 0.99 && s[2] == -1.0);
    }

    #[test]
    fn unsupported_and_broken_audio_is_an_error() {
        assert!(decode(b"OggS....", "a.ogg").unwrap_err().contains("Ogg"));
        assert!(decode(b"fLaC....", "a.flac").unwrap_err().contains("FLAC"));
        assert!(decode(b"\0\0\0\x20ftypM4A ", "a.m4a").unwrap_err().contains("MP4"));
        assert!(decode(b"hello", "a.txt").unwrap_err().contains("unsupported"));
        assert!(decode(&wav16(16_000, 1, &[0; 100]), "a.wav")
            .unwrap_err()
            .contains("shorter"));
        assert!(decode(b"RIFF\0\0\0\0WAVEjunk", "a.wav").is_err());
    }

    fn transcript() -> Transcript {
        Transcript {
            task: "transcribe",
            language: "english".into(),
            duration: 7.5,
            temperature: 0.0,
            segments: vec![
                Segment {
                    start: 0.0,
                    end: 3.5,
                    text: " Hello there.".into(),
                    no_speech_prob: 0.01,
                },
                Segment {
                    start: 3.5,
                    end: 3723.456,
                    text: " General Kenobi.".into(),
                    no_speech_prob: 0.02,
                },
            ],
        }
    }

    #[test]
    fn formats_the_transcript() {
        let t = transcript();
        assert_eq!(
            t.render(ResponseFormat::Json),
            r#"{"text":"Hello there. General Kenobi."}"#
        );
        assert_eq!(t.render(ResponseFormat::Text), "Hello there. General Kenobi.\n");
        assert_eq!(
            t.render(ResponseFormat::Srt),
            "1\n00:00:00,000 --> 00:00:03,500\nHello there.\n\n2\n00:00:03,500 --> 01:02:03,456\nGeneral Kenobi.\n\n"
        );
        assert_eq!(
            t.render(ResponseFormat::Vtt),
            "WEBVTT\n\n00:00:00.000 --> 00:00:03.500\nHello there.\n\n00:00:03.500 --> 01:02:03.456\nGeneral Kenobi.\n\n"
        );
        let v: Value = serde_json::from_str(&t.render(ResponseFormat::VerboseJson)).unwrap();
        assert_eq!(v["task"], "transcribe");
        assert_eq!(v["language"], "english");
        assert_eq!(v["duration"], 7.5);
        assert_eq!(v["segments"][1]["id"], 1);
        assert_eq!(v["segments"][1]["end"], 3723.456);
        assert_eq!(v["segments"][0]["text"], " Hello there.");
    }

    #[test]
    fn response_format_names() {
        assert_eq!(ResponseFormat::parse("SRT").unwrap(), ResponseFormat::Srt);
        assert!(ResponseFormat::parse("xml").is_err());
        assert_eq!(ResponseFormat::Vtt.content_type(), "text/plain; charset=utf-8");
        assert_eq!(ResponseFormat::VerboseJson.content_type(), "application/json");
    }
}

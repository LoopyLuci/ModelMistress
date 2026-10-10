//! Speech to text with whisper.cpp built into the binary (feature `whisper`). A whisper model is a GGML file
//! (`ggml-base.en.bin`, `ggml-large-v3-turbo-q5_0.bin`, ...) in `<home>/whisper`, `<home>/models`, or one of the
//! folders in `model_dirs`. Finding one by name needs no feature, so the hub can say 404 for an unknown model.

use std::path::{Path, PathBuf};

use crate::audio::{Request, Transcript};
use crate::engine::{Fail, Result};

/// Whether this build can transcribe.
pub const AVAILABLE: bool = cfg!(feature = "whisper");

/// Name given to the models in the API: the file name without `ggml-` and `.bin` (`ggml-base.en.bin` is `base.en`).
pub fn model_name(path: &Path) -> Option<String> {
    let f = path.file_name()?.to_string_lossy().to_lowercase();
    let stem = f.strip_suffix(".bin")?;
    Some(stem.strip_prefix("ggml-")?.to_string()).filter(|s| !s.is_empty())
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() && depth > 1 => walk(&p, depth - 1, out),
            Ok(m) if m.is_file() => {
                if let Some(n) = model_name(&p) {
                    out.push((n, p));
                }
            }
            _ => {}
        }
    }
}

/// Every whisper model under `dirs` (name, path), best-known name first, no duplicates.
pub fn list_models(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for d in dirs {
        walk(d, 3, &mut found);
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

/// The model a request names. An existing file path works as is. "" and "whisper-1" (what OpenAI clients send)
/// mean `preferred` (the `whisper_model` setting), else the only model, else the first of a sensible list.
pub fn find_model(dirs: &[PathBuf], name: &str, preferred: Option<&str>) -> Result<PathBuf> {
    let name = name.trim();
    if !name.is_empty() && Path::new(name).is_file() {
        return Ok(PathBuf::from(name));
    }
    let models = list_models(dirs);
    let generic = name.is_empty() || name.eq_ignore_ascii_case("whisper-1") || name.eq_ignore_ascii_case("whisper");
    let want = if generic {
        preferred.unwrap_or("").trim().to_lowercase()
    } else {
        name.to_lowercase()
    };
    let want = want
        .strip_prefix("ggml-")
        .unwrap_or(&want)
        .trim_end_matches(".bin")
        .to_string();
    let by_name = |n: &str| models.iter().find(|(m, _)| m == n).map(|(_, p)| p.clone());
    if !want.is_empty() {
        if let Some(p) = by_name(&want) {
            return Ok(p);
        }
        let prefixed: Vec<_> = models.iter().filter(|(m, _)| m.starts_with(&want)).collect();
        if let [(_, p)] = prefixed.as_slice() {
            return Ok(p.clone());
        }
        if !generic {
            return Err(Fail::not_found(format!(
                "no whisper model \"{name}\" (found: {}). Put a ggml-*.bin file in a model folder.",
                if models.is_empty() {
                    "none".to_string()
                } else {
                    models.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>().join(", ")
                }
            )));
        }
    }
    if let [(_, p)] = models.as_slice() {
        return Ok(p.clone());
    }
    for n in ["small", "small.en", "base", "base.en", "tiny", "tiny.en"] {
        if let Some(p) = by_name(n) {
            return Ok(p);
        }
    }
    models.into_iter().next().map(|(_, p)| p).ok_or_else(|| {
        Fail::not_found(
            "no whisper model on this machine: put a ggml-*.bin file (for example ggml-base.en.bin from \
huggingface.co/ggerganov/whisper.cpp) in <home>/whisper or a folder listed in model_dirs",
        )
    })
}

/// Run `req` on already decoded 16 kHz mono `samples`. `translate` turns the speech into English text.
#[cfg(feature = "whisper")]
pub fn transcribe(model: &Path, samples: &[f32], req: &Request, translate: bool) -> Result<Transcript> {
    use std::sync::{Arc, Mutex, OnceLock};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    // One loaded model is kept: the next request for it skips reading the file again.
    type Slot = Mutex<Option<(PathBuf, Arc<WhisperContext>)>>;
    static LOADED: OnceLock<Slot> = OnceLock::new();
    static QUIET: std::sync::Once = std::sync::Once::new();
    QUIET.call_once(whisper_rs::install_logging_hooks);

    let ctx = {
        let mut slot = LOADED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match slot.as_ref().filter(|(p, _)| p == model) {
            Some((_, c)) => c.clone(),
            None => {
                let c = Arc::new(
                    WhisperContext::new_with_params(model, WhisperContextParameters::default())
                        .map_err(|e| Fail::new(500, "model_error", format!("{}: {e}", model.display())))?,
                );
                *slot = Some((model.to_path_buf(), c.clone()));
                c
            }
        }
    };
    if translate && !ctx.is_multilingual() {
        return Err(Fail::invalid(
            "this whisper model is English only and cannot translate; use a multilingual model (without .en)",
        ));
    }
    let language = match req.language.as_deref() {
        Some(l) if whisper_rs::get_lang_id(l).is_none() => {
            return Err(Fail::invalid(format!("language \"{l}\" is not a known ISO-639-1 code")))
        }
        l => l,
    };
    if language.is_some_and(|l| l != "en") && !ctx.is_multilingual() {
        return Err(Fail::invalid(
            "this whisper model is English only; use a multilingual model for other languages",
        ));
    }

    let mut state = ctx
        .create_state()
        .map_err(|e| Fail::new(500, "model_error", e.to_string()))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(
        std::thread::available_parallelism()
            .map(|n| n.get().min(8))
            .unwrap_or(4) as i32,
    );
    params.set_translate(translate);
    // No language: English for an English-only model, else detect it from the audio.
    match language {
        Some(l) => params.set_language(Some(l)),
        None if ctx.is_multilingual() => {
            params.set_language(Some("auto"));
            params.set_detect_language(false);
        }
        None => params.set_language(Some("en")),
    }
    if let Some(p) = &req.prompt {
        params.set_initial_prompt(p);
    }
    params.set_temperature(req.temperature);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, samples)
        .map_err(|e| Fail::new(500, "inference_error", format!("whisper failed: {e}")))?;

    let lang_code = whisper_rs::get_lang_str_full(state.full_lang_id_from_state()).unwrap_or("english");
    let segments = state
        .as_iter()
        .map(|s| crate::audio::Segment {
            start: s.start_timestamp() as f64 / 100.0,
            end: s.end_timestamp() as f64 / 100.0,
            text: s.to_str_lossy().map(|t| t.into_owned()).unwrap_or_default(),
            no_speech_prob: s.no_speech_probability(),
        })
        .collect();
    Ok(Transcript {
        task: if translate { "translate" } else { "transcribe" },
        language: if translate {
            "english".to_string()
        } else {
            lang_code.to_string()
        },
        duration: samples.len() as f64 / crate::audio::SAMPLE_RATE as f64,
        segments,
        temperature: req.temperature,
    })
}

#[cfg(not(feature = "whisper"))]
pub fn transcribe(_: &Path, _: &[f32], _: &Request, _: bool) -> Result<Transcript> {
    Err(Fail::new(501, "not_implemented", NOT_BUILT))
}

pub const NOT_BUILT: &str = "this build of ModelMistress has no speech to text; rebuild with `--features whisper`";

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(names: &[&str]) -> tempdir::Dir {
        let d = tempdir::Dir::new();
        for n in names {
            std::fs::write(d.0.join(n), b"x").unwrap();
        }
        d
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("mm-speech-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn names_come_from_file_names() {
        assert_eq!(model_name(Path::new("a/ggml-base.en.bin")).as_deref(), Some("base.en"));
        assert_eq!(
            model_name(Path::new("GGML-Large-V3-Q5_0.BIN")).as_deref(),
            Some("large-v3-q5_0")
        );
        assert_eq!(model_name(Path::new("model.bin")), None);
        assert_eq!(model_name(Path::new("ggml-base.gguf")), None);
    }

    #[test]
    fn finds_models_by_name() {
        let d = dir_with(&["ggml-base.en.bin", "ggml-tiny.bin", "ggml-tiny.en.bin", "notes.txt"]);
        let dirs = [d.0.clone()];
        assert!(find_model(&dirs, "base.en", None)
            .unwrap()
            .ends_with("ggml-base.en.bin"));
        assert!(find_model(&dirs, "ggml-tiny.bin", None)
            .unwrap()
            .ends_with("ggml-tiny.bin"));
        assert!(
            find_model(&dirs, "base", None).unwrap().ends_with("ggml-base.en.bin"),
            "a unique prefix"
        );
        assert!(
            find_model(&dirs, "tiny", None).unwrap().ends_with("ggml-tiny.bin"),
            "exact beats prefix"
        );
        // what OpenAI clients send
        assert!(find_model(&dirs, "whisper-1", None)
            .unwrap()
            .ends_with("ggml-base.en.bin"));
        assert!(find_model(&dirs, "", Some("tiny.en"))
            .unwrap()
            .ends_with("ggml-tiny.en.bin"));
        let e = find_model(&dirs, "large-v3", None).unwrap_err();
        assert_eq!(e.status, 404);
        assert!(e.message.contains("base.en"));
    }

    #[test]
    fn no_models_is_a_404_and_a_path_works() {
        let d = dir_with(&[]);
        assert_eq!(
            find_model(std::slice::from_ref(&d.0), "whisper-1", None)
                .unwrap_err()
                .status,
            404
        );
        assert_eq!(
            find_model(std::slice::from_ref(&d.0), "", None).unwrap_err().status,
            404
        );
        let f = d.0.join("mine.bin");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(find_model(&[], f.to_str().unwrap(), None).unwrap(), f);
    }
}

//! The Cactus runtime: Needle 3 (tool calling, extraction), Whistle (speech to text) and Mirage (vision).
//!
//! The runtime is a small C library with no model handle: one process-wide state, so every call goes through one lock
//! (the Python package serializes the same way). `DllRuntime` loads the shared library with `libloading` from
//! `CACTUS_LIB_PATH` (a file or a folder) or `<home>/engines/cactus/`; the functions are the ones in the release's
//! `cactus.h`. A missing library is a normal state, reported as "unavailable" with the fix, never a crash.
//!
//! Checked against the real runtime (the Windows x64 `libcactus.dll` of the pinned wheel, with Needle 3, Whistle and
//! Mirage): the exported names match, `cactus_load` takes the archive's bytes, and `cactus_init` plus `cactus_complete`
//! answer a tool call as a JSON string in the caller's buffer (`function_calls`, `suppressed_calls`, `confidence`,
//! `reasoning`). Negative means an error and `cactus_last_error` says why. Not checked: other platforms' libraries, and
//! the speech and vision calls beyond the one-call probes of `model-mistress misty doctor`.

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};

pub const TEXT: u32 = 1;
pub const SPEECH: u32 = 2;
pub const VISION: u32 = 4;

/// The `.cact` archives Misty looks for, by role.
pub const ARCHIVES: [(&str, &str, u32); 3] = [
    ("reflex", "needle3.cact", TEXT),
    ("ear", "whistle.cact", SPEECH),
    ("eye", "mirage.cact", VISION),
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CactusStatus {
    pub available: bool,
    pub library: Option<PathBuf>,
    /// Bitmask of what is loaded: 1 text, 2 speech, 4 vision.
    pub models: u32,
    pub error: Option<String>,
}

impl CactusStatus {
    pub fn has(&self, mask: u32) -> bool {
        self.available && self.models & mask != 0
    }
}

pub trait CactusRuntime: Send + Sync {
    fn status(&self) -> CactusStatus;
    /// Load one `.cact` archive; returns the bitmask of everything loaded now.
    fn load(&self, archive: &Path) -> Result<u32, String>;
    /// One Needle turn: the model picks calls from `tools` (JSON array of `{name, description, parameters}`).
    fn tool_call(&self, system: &str, tools: &Value, input: &str, max_new_tokens: i32) -> Result<Value, String>;
    fn transcribe(
        &self,
        pcm: &[f32],
        language: Option<&str>,
        keywords: Option<&str>,
        words: bool,
    ) -> Result<Value, String>;
    fn describe(&self, rgb: &[u8], width: i32, height: i32, options: Option<&str>) -> Result<Value, String>;
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
    fn set_threads(&self, threads: i32);
}

// ---- the real library --------------------------------------------------------------------------------------------

type LoadFn = unsafe extern "C" fn(*const u8, u64) -> c_int;
type ModelsFn = unsafe extern "C" fn() -> c_int;
type ErrorFn = unsafe extern "C" fn() -> *const c_char;
type InitFn = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> c_int;
type CompleteFn = unsafe extern "C" fn(*const c_char, *const f32, c_int, c_int, *mut c_char, c_int) -> c_int;
type ThreadsFn = unsafe extern "C" fn(c_int);
type ResetFn = unsafe extern "C" fn();
type TranscribeFn =
    unsafe extern "C" fn(*const f32, c_int, *const c_char, *const c_char, c_int, *mut c_char, c_int) -> c_int;
type DescribeFn = unsafe extern "C" fn(
    *const u8,
    c_int,
    c_int,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_char,
    c_int,
) -> c_int;
type EmbedFn = unsafe extern "C" fn(*const c_char, *const f32, c_int, *mut f32, c_int) -> c_int;

struct Lib {
    // kept alive for as long as the function pointers below are used
    _lib: libloading::Library,
    path: PathBuf,
    load: LoadFn,
    models: ModelsFn,
    last_error: ErrorFn,
    init: InitFn,
    complete: CompleteFn,
    set_threads: ThreadsFn,
    reset: ResetFn,
    transcribe: TranscribeFn,
    describe: DescribeFn,
    embed: EmbedFn,
}

pub struct DllRuntime {
    search: Vec<PathBuf>,
    lib: Mutex<Option<Lib>>,
}

const NAMES: [&str; 7] = [
    "libcactus.dll", // what the Windows wheel ships
    "cactus.dll",
    "libcactus.so",
    "libcactus.dylib",
    "cactus_runtime.dll",
    "libcactus_runtime.so",
    "libcactus_runtime.dylib",
];

/// A shared library whose name mentions cactus (`libcactus-1.2.dll`, `cactus_native.so`...), for releases that rename it.
fn looks_like_cactus(p: &Path) -> bool {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.contains("cactus")
        && [".dll", ".so", ".dylib"]
            .iter()
            .any(|e| name.ends_with(e) || name.contains(&format!("{e}.")))
}

fn find_in(dir: &Path) -> Option<PathBuf> {
    for name in NAMES {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut named: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && looks_like_cactus(p))
        .collect();
    named.sort();
    named.into_iter().next()
}

fn find_library(search: &[PathBuf]) -> Option<PathBuf> {
    for base in search {
        if base.is_file() {
            return Some(base.clone());
        }
        if let Some(p) = find_in(base) {
            return Some(p);
        }
        // the wheel unpacks the library into a `cactus_runtime` folder: look one level down
        if let Ok(rd) = std::fs::read_dir(base) {
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for d in dirs {
                if let Some(p) = find_in(&d) {
                    return Some(p);
                }
            }
        }
    }
    None
}

impl DllRuntime {
    /// `home` is the ModelMistress home. `CACTUS_LIB_PATH` (also read by Cactus's own tools) comes first.
    pub fn new(home: &Path) -> Self {
        let mut search = vec![];
        if let Some(p) = std::env::var_os("CACTUS_LIB_PATH") {
            search.push(PathBuf::from(p));
        }
        search.push(home.join("engines").join("cactus"));
        Self {
            search,
            lib: Mutex::new(None),
        }
    }

    fn open(&self) -> Result<(), String> {
        let mut guard = self.lib.lock();
        if guard.is_some() {
            return Ok(());
        }
        let path = find_library(&self.search).ok_or_else(|| {
            format!(
                "the Cactus runtime library was not found (looked in {}); run `model-mistress misty setup` or set CACTUS_LIB_PATH",
                self.search.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            )
        })?;
        // SAFETY: loading a library runs its initializers; this is the runtime the owner installed.
        let lib = unsafe { libloading::Library::new(&path) }.map_err(|e| format!("{}: {e}", path.display()))?;
        macro_rules! sym {
            ($name:literal, $t:ty) => {
                // SAFETY: the symbol is declared by the release's cactus.h with this signature.
                *unsafe { lib.get::<$t>(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| format!("{}: missing {}: {e}", path.display(), $name))?
            };
        }
        let l = Lib {
            load: sym!("cactus_load", LoadFn),
            models: sym!("cactus_models", ModelsFn),
            last_error: sym!("cactus_last_error", ErrorFn),
            init: sym!("cactus_init", InitFn),
            complete: sym!("cactus_complete", CompleteFn),
            set_threads: sym!("cactus_set_threads", ThreadsFn),
            reset: sym!("cactus_reset", ResetFn),
            transcribe: sym!("cactus_transcribe", TranscribeFn),
            describe: sym!("cactus_describe", DescribeFn),
            embed: sym!("cactus_embed", EmbedFn),
            path,
            _lib: lib,
        };
        *guard = Some(l);
        Ok(())
    }
}

fn last_error(l: &Lib) -> String {
    // SAFETY: the library returns a NUL-terminated string it owns, or null.
    let p = unsafe { (l.last_error)() };
    if p.is_null() {
        "unknown error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// The tools as Needle should read them.
///
/// Needle reads its tools as text, and the order of the keys changes what it picks. The JSON `serde_json` writes sorts
/// keys alphabetically (`description` before `name`); measured on the real Needle3 with Misty's fifteen tools and 27
/// labelled requests, that order picked the right tool for 11 of 21 requests (median confidence 0.03), while
/// `name`, `description`, `parameters` with `type`, `properties`, `required` inside picked 15 (median 0.33). Spacing made
/// no difference. So the text is written by hand, in the order Needle was shown.
pub fn needle_json(tools: &Value) -> String {
    fn prop(v: &Value) -> String {
        let Some(o) = v.as_object() else { return v.to_string() };
        let mut parts: Vec<String> = ["type", "description"]
            .iter()
            .filter_map(|k| o.get(*k).map(|x| format!("{}:{x}", Value::from(*k))))
            .collect();
        parts.extend(
            o.iter()
                .filter(|(k, _)| *k != "type" && *k != "description")
                .map(|(k, x)| format!("{}:{x}", Value::from(k.as_str()))),
        );
        format!("{{{}}}", parts.join(","))
    }
    fn params(p: &Value) -> String {
        let props: Vec<String> = p["properties"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| format!("{}:{}", Value::from(k.as_str()), prop(v)))
                    .collect()
            })
            .unwrap_or_default();
        let required = if p["required"].is_array() {
            p["required"].clone()
        } else {
            json!([])
        };
        let kind = p.get("type").cloned().unwrap_or_else(|| json!("object"));
        format!(
            "{{\"type\":{kind},\"properties\":{{{}}},\"required\":{required}}}",
            props.join(",")
        )
    }
    let list: Vec<String> = tools
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            format!(
                "{{\"name\":{},\"description\":{},\"parameters\":{}}}",
                t["name"],
                t.get("description").cloned().unwrap_or_else(|| json!("")),
                params(&t["parameters"])
            )
        })
        .collect();
    format!("[{}]", list.join(","))
}

fn cstr(s: &str) -> Result<CString, String> {
    CString::new(s.replace('\0', "")).map_err(|e| e.to_string())
}

fn opt_cstr(s: Option<&str>) -> Result<Option<CString>, String> {
    s.map(cstr).transpose()
}

fn ptr(c: &Option<CString>) -> *const c_char {
    c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null())
}

/// The JSON a call wrote into its buffer.
fn read_out(buf: &[u8], rc: c_int, l: &Lib) -> Result<Value, String> {
    if rc < 0 {
        return Err(last_error(l));
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let text = String::from_utf8_lossy(&buf[..end]);
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "the Cactus runtime answered something that is not JSON ({e}): {}",
            text.chars().take(200).collect::<String>()
        )
    })
}

const OUT: usize = 64 * 1024;

impl CactusRuntime for DllRuntime {
    fn status(&self) -> CactusStatus {
        if let Err(e) = self.open() {
            return CactusStatus {
                available: false,
                library: None,
                models: 0,
                error: Some(e),
            };
        }
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        // SAFETY: no arguments, returns a bitmask.
        let models = unsafe { (l.models)() }.max(0) as u32;
        CactusStatus {
            available: true,
            library: Some(l.path.clone()),
            models,
            error: None,
        }
    }

    fn load(&self, archive: &Path) -> Result<u32, String> {
        self.open()?;
        let bytes = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        // SAFETY: the pointer and length describe `bytes`, which outlives the call.
        let rc = unsafe { (l.load)(bytes.as_ptr(), bytes.len() as u64) };
        if rc < 0 {
            return Err(format!("{}: {}", archive.display(), last_error(l)));
        }
        Ok(unsafe { (l.models)() }.max(0) as u32)
    }

    fn tool_call(&self, system: &str, tools: &Value, input: &str, max_new_tokens: i32) -> Result<Value, String> {
        self.open()?;
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        let (sys, tl, inp) = (cstr(system)?, cstr(&needle_json(tools))?, cstr(input)?);
        // SAFETY: all pointers are valid NUL-terminated strings for the duration of the calls.
        unsafe {
            if (l.init)(sys.as_ptr(), tl.as_ptr(), std::ptr::null()) < 0 {
                return Err(last_error(l));
            }
            let mut out = vec![0u8; OUT];
            let rc = (l.complete)(
                inp.as_ptr(),
                std::ptr::null(),
                0,
                max_new_tokens,
                out.as_mut_ptr() as *mut c_char,
                OUT as c_int,
            );
            read_out(&out, rc, l)
        }
    }

    fn transcribe(
        &self,
        pcm: &[f32],
        language: Option<&str>,
        keywords: Option<&str>,
        words: bool,
    ) -> Result<Value, String> {
        self.open()?;
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        let (lang, kw) = (opt_cstr(language)?, opt_cstr(keywords)?);
        let mut out = vec![0u8; OUT];
        // SAFETY: `pcm` and the strings outlive the call; `out` has OUT bytes.
        let rc = unsafe {
            (l.transcribe)(
                pcm.as_ptr(),
                pcm.len() as c_int,
                ptr(&lang),
                ptr(&kw),
                words as c_int,
                out.as_mut_ptr() as *mut c_char,
                OUT as c_int,
            )
        };
        read_out(&out, rc, l)
    }

    fn describe(&self, rgb: &[u8], width: i32, height: i32, options: Option<&str>) -> Result<Value, String> {
        self.open()?;
        if rgb.len() != (width as usize) * (height as usize) * 3 {
            return Err("describe needs packed RGB: width x height x 3 bytes".into());
        }
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        let opts = opt_cstr(options)?;
        let mut out = vec![0u8; 4 * OUT];
        // SAFETY: `rgb` has width*height*3 bytes (checked above); the rest as for transcribe.
        let rc = unsafe {
            (l.describe)(
                rgb.as_ptr(),
                width,
                height,
                ptr(&opts),
                std::ptr::null(),
                std::ptr::null(),
                out.as_mut_ptr() as *mut c_char,
                out.len() as c_int,
            )
        };
        read_out(&out, rc, l)
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.open()?;
        let guard = self.lib.lock();
        let l = guard.as_ref().expect("opened above");
        let inp = cstr(text)?;
        let mut out = vec![0f32; 4096];
        // SAFETY: `out` has 4096 floats.
        let n = unsafe { (l.embed)(inp.as_ptr(), std::ptr::null(), 0, out.as_mut_ptr(), out.len() as c_int) };
        if n <= 0 {
            return Err(last_error(l));
        }
        out.truncate((n as usize).min(4096));
        Ok(out)
    }

    fn set_threads(&self, threads: i32) {
        if self.open().is_ok() {
            if let Some(l) = self.lib.lock().as_ref() {
                // SAFETY: plain integer argument.
                unsafe { (l.set_threads)(threads) };
                unsafe { (l.reset)() };
            }
        }
    }
}

// ---- what Misty asks of it -------------------------------------------------------------------------------------

/// Where the `.cact` archives live: `<home>/models/cactus/`.
pub fn archive_dir(home: &Path) -> PathBuf {
    home.join("models").join("cactus")
}

/// Loads every archive that is present and not loaded yet. Returns (role, file, loaded-or-error) per archive.
pub fn load_archives(rt: &dyn CactusRuntime, home: &Path) -> Vec<(String, String, Result<(), String>)> {
    let dir = archive_dir(home);
    let mut out = vec![];
    for (role, file, mask) in ARCHIVES {
        let p = dir.join(file);
        if !p.is_file() {
            continue;
        }
        if rt.status().models & mask != 0 {
            out.push((role.to_string(), file.to_string(), Ok(())));
            continue;
        }
        out.push((role.to_string(), file.to_string(), rt.load(&p).map(|_| ())));
    }
    out
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Decision {
    pub calls: Vec<ToolCall>,
    /// Needle's calibrated score; `None` for weights that were not trained with the head.
    pub confidence: Option<f32>,
    /// A call the engine held back (confidence under 0.1 or a grounding check).
    pub suppressed: Vec<ToolCall>,
    pub reasoning: String,
}

fn calls_of(v: Option<&Value>) -> Vec<ToolCall> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(ToolCall {
                        name: c.get("name")?.as_str()?.to_string(),
                        arguments: c.get("arguments").cloned().unwrap_or_else(|| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ask Needle which tools to call for `input`. `tools` use OpenAI's `{type: function, function: {...}}` shape or
/// Needle's flat `{name, description, parameters}`; they are flattened here.
pub fn decide(rt: &dyn CactusRuntime, system: &str, tools: &[Value], input: &str) -> Result<Decision, String> {
    let flat: Vec<Value> = tools
        .iter()
        .map(|t| t.get("function").cloned().unwrap_or_else(|| t.clone()))
        .collect();
    let v = rt.tool_call(system, &Value::Array(flat), input, 512)?;
    if v.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Needle failed")
            .to_string());
    }
    Ok(Decision {
        calls: calls_of(v.get("function_calls")),
        confidence: v.get("confidence").and_then(Value::as_f64).map(|c| c as f32),
        suppressed: calls_of(v.get("suppressed_calls")),
        reasoning: v.get("reasoning").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

/// WAV bytes to 16 kHz mono samples (Whistle's input), by linear interpolation when the file has another rate.
pub fn pcm16k(wav: &[u8]) -> Result<Vec<f32>, String> {
    let (samples, rate) = crate::audio::decode_wav(wav)?;
    if rate == 16_000 || rate == 0 {
        return Ok(samples);
    }
    let ratio = rate as f64 / 16_000.0;
    let n = (samples.len() as f64 / ratio) as usize;
    Ok((0..n)
        .map(|i| {
            let x = i as f64 * ratio;
            let (a, f) = (x as usize, x.fract() as f32);
            let b = (a + 1).min(samples.len().saturating_sub(1));
            samples[a.min(samples.len() - 1)] * (1.0 - f) + samples[b] * f
        })
        .collect())
}

/// What Mirage saw, as a few words a language model can read.
///
/// Mirage's own result carries a polygon mask and a box for every object: on a picture of two shapes and one word that
/// was 2,000 characters, over 1,000 tokens, and a 9B reads about 30 tokens a second on this CPU (50 s for the answer).
/// The model needs the names, where they are, the text that was read and the scene. The run's `vision` event keeps the
/// whole result for a program that wants the boxes.
pub fn digest(v: &Value) -> String {
    if let Some(s) = v.get("description").and_then(Value::as_str) {
        return s.to_string();
    }
    let mut parts: Vec<String> = vec![];
    let objects: Vec<String> = v["objects"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let name = o["name"].as_str().unwrap_or("object");
            match o["position"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => format!("{name} ({p})"),
                None => name.to_string(),
            }
        })
        .collect();
    if !objects.is_empty() {
        parts.push(format!("objects: {}", objects.join(", ")));
    }
    let texts: Vec<String> = v["text"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["string"].as_str().map(|s| format!("\"{s}\"")))
        .collect();
    if !texts.is_empty() {
        parts.push(format!("text read in the image: {}", texts.join(", ")));
    }
    if let Some(scene) = v["scene"].as_object() {
        let s: Vec<String> = scene
            .iter()
            .filter_map(|(k, x)| Some(format!("{k} {}", x.as_str()?)))
            .collect();
        if !s.is_empty() {
            parts.push(format!("scene: {}", s.join(", ")));
        }
    }
    for k in ["events", "relations", "regions"] {
        if v[k].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            parts.push(format!("{k}: {}", v[k]));
        }
    }
    if parts.is_empty() {
        // an answer in a shape not seen before: the start of it, not all of it
        return v.to_string().chars().take(1500).collect();
    }
    parts.join("; ")
}

/// Decode an image file's bytes (PNG or JPEG) to packed RGB.
pub fn rgb_of(bytes: &[u8]) -> Result<(Vec<u8>, i32, i32), String> {
    let img = ::image::load_from_memory(bytes)
        .map_err(|e| format!("cannot read the image (PNG and JPEG are supported): {e}"))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w as i32, h as i32))
}

// ---- a scripted runtime for tests -----------------------------------------------------------------------------

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::VecDeque;

    /// Answers what a test queued, in order, and records what it was asked.
    #[derive(Default)]
    pub struct Fake {
        pub models: Mutex<u32>,
        pub tool_replies: Mutex<VecDeque<Value>>,
        pub transcripts: Mutex<VecDeque<String>>,
        pub seen_tools: Mutex<Vec<(String, Value, String)>>,
        pub unavailable: bool,
    }

    impl Fake {
        pub fn with_models(mask: u32) -> Self {
            Self {
                models: Mutex::new(mask),
                ..Default::default()
            }
        }
        pub fn queue_call(&self, name: &str, args: Value, confidence: f64) {
            self.tool_replies
                .lock()
                .push_back(json!({"success": true, "type": "call",
                "function_calls": [{"name": name, "arguments": args}], "confidence": confidence, "reasoning": "test"}));
        }
        pub fn queue_none(&self) {
            self.tool_replies
                .lock()
                .push_back(json!({"success": true, "type": "call", "function_calls": [], "confidence": 0.9}));
        }
    }

    impl CactusRuntime for Fake {
        fn status(&self) -> CactusStatus {
            if self.unavailable {
                return CactusStatus {
                    available: false,
                    library: None,
                    models: 0,
                    error: Some("no runtime".into()),
                };
            }
            CactusStatus {
                available: true,
                library: Some(PathBuf::from("fake")),
                models: *self.models.lock(),
                error: None,
            }
        }
        fn load(&self, archive: &Path) -> Result<u32, String> {
            let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let mask = ARCHIVES
                .iter()
                .find(|a| a.1 == name)
                .map(|a| a.2)
                .ok_or("not a known archive")?;
            *self.models.lock() |= mask;
            Ok(*self.models.lock())
        }
        fn tool_call(&self, system: &str, tools: &Value, input: &str, _: i32) -> Result<Value, String> {
            self.seen_tools
                .lock()
                .push((system.to_string(), tools.clone(), input.to_string()));
            self.tool_replies
                .lock()
                .pop_front()
                .ok_or_else(|| "no scripted reply".to_string())
        }
        fn transcribe(&self, pcm: &[f32], _: Option<&str>, _: Option<&str>, _: bool) -> Result<Value, String> {
            let text = self
                .transcripts
                .lock()
                .pop_front()
                .unwrap_or_else(|| format!("{} samples", pcm.len()));
            Ok(json!({"text": text, "language": "en"}))
        }
        fn describe(&self, _: &[u8], w: i32, h: i32, _: Option<&str>) -> Result<Value, String> {
            Ok(json!({"description": format!("a {w}x{h} picture"), "objects": []}))
        }
        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            Ok(vec![text.len() as f32, 1.0])
        }
        fn set_threads(&self, _: i32) {}
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    #[test]
    fn a_missing_library_is_a_clear_unavailable_state() {
        let d = crate::testing::TestDir::new("misty-cactus");
        let rt = DllRuntime::new(&d);
        let s = rt.status();
        assert!(!s.available);
        let e = s.error.unwrap();
        assert!(e.contains("not found") && e.contains("misty setup"), "{e}");
        assert!(rt.tool_call("", &json!([]), "x", 8).is_err());
        assert!(rt.load(&d.join("needle3.cact")).is_err());
    }

    #[test]
    fn the_library_is_found_in_the_home_folder_or_one_level_down() {
        let d = crate::testing::TestDir::new("misty-cactus2");
        let base = d.join("engines").join("cactus");
        std::fs::create_dir_all(base.join("cactus_runtime")).unwrap();
        assert_eq!(find_library(std::slice::from_ref(&base)), None);
        let lib = base.join("cactus_runtime").join("cactus.dll");
        std::fs::write(&lib, b"x").unwrap();
        assert_eq!(find_library(std::slice::from_ref(&base)), Some(lib));
        // a file that is not a real library gives an error that names it, not a panic
        let rt = DllRuntime {
            search: vec![base],
            lib: Mutex::new(None),
        };
        let e = rt.status().error.unwrap();
        assert!(e.contains("cactus.dll"), "{e}");
    }

    #[test]
    fn needle_reads_its_tools_in_the_order_it_was_shown_and_nothing_is_lost() {
        let tools = json!([
            {"name": "fs_read", "description": "Read a text file.", "class": "read",
             "parameters": {"type": "object", "required": ["path"],
                            "properties": {"path": {"description": "file", "type": "string"}, "end_line": {"type": "integer"}}}},
            {"name": "datetime", "description": "Now.", "parameters": {"type": "object", "properties": {}, "required": []}}
        ]);
        let text = needle_json(&tools);
        // name before description before parameters; type, properties, required inside; type before description
        assert_eq!(
            text,
            r#"[{"name":"fs_read","description":"Read a text file.","parameters":{"type":"object","properties":{"end_line":{"type":"integer"},"path":{"type":"string","description":"file"}},"required":["path"]}},{"name":"datetime","description":"Now.","parameters":{"type":"object","properties":{},"required":[]}}]"#
        );
        // it is the same tools: parsing it back loses nothing Needle needs
        let back: Value = serde_json::from_str(&text).unwrap();
        for (a, b) in tools.as_array().unwrap().iter().zip(back.as_array().unwrap()) {
            assert_eq!(
                (&a["name"], &a["description"], &a["parameters"]),
                (&b["name"], &b["description"], &b["parameters"])
            );
        }
        // quotes and newlines in a description stay valid JSON
        let odd = needle_json(&json!([{"name": "x", "description": "say \"hi\"\nnow", "parameters": {}}]));
        assert!(serde_json::from_str::<Value>(&odd).is_ok(), "{odd}");
        assert_eq!(needle_json(&json!([])), "[]");
    }

    #[test]
    fn what_mirage_saw_is_a_line_for_the_model_without_masks_and_boxes() {
        // the shape of a real answer (two shapes and a word), masks shortened
        let seen = json!({
            "events": [], "regions": [], "relations": [],
            "objects": [
                {"bbox": [131, 222, 504, 705], "description": {"color": "red", "shape": "circle"}, "id": 1,
                 "mask": [262, 220, 254, 227, 238, 230], "name": "red circle", "position": "left", "score": 0.851},
                {"bbox": [583, 384, 880, 733], "description": {"color": "blue", "shape": "square"}, "id": 2,
                 "mask": [598, 385, 593, 391], "name": "blue square", "position": "right", "score": 0.7}],
            "reasoning": "lang: en | red circle, blue square",
            "scene": {"background": "white", "style": "minimalist"},
            "text": [{"bbox": [312, 797, 683, 923], "score": 0.719, "string": "FALCON"}]
        });
        let line = digest(&seen);
        assert_eq!(
            line,
            "objects: red circle (left), blue square (right); text read in the image: \"FALCON\"; scene: background white, style minimalist"
        );
        assert!(!line.contains("262") && !line.contains("bbox"));
        // a plain description is used as it is; an unknown shape is cut, not dropped
        assert_eq!(
            digest(&json!({"description": "a 64x64 picture", "objects": []})),
            "a 64x64 picture"
        );
        assert!(digest(&json!({"something": "else"})).contains("something"));
    }

    #[test]
    fn a_png_becomes_packed_rgb_in_every_build_and_garbage_is_an_error() {
        let img = ::image::RgbImage::from_pixel(3, 2, ::image::Rgb([255, 0, 0]));
        let mut png = std::io::Cursor::new(vec![]);
        img.write_to(&mut png, ::image::ImageFormat::Png).unwrap();
        let (rgb, w, h) = rgb_of(&png.into_inner()).unwrap();
        assert_eq!((w, h, rgb.len()), (3, 2, 18));
        assert_eq!(&rgb[..3], &[255, 0, 0]);
        assert!(rgb_of(b"not an image").unwrap_err().contains("cannot read the image"));
    }

    #[test]
    fn the_windows_wheel_name_and_renamed_libraries_are_found() {
        let d = crate::testing::TestDir::new("misty-cactus3");
        let base = d.join("engines").join("cactus");
        std::fs::create_dir_all(&base).unwrap();
        // not a library: other files in the folder are never picked
        std::fs::write(base.join("notes.txt"), b"x").unwrap();
        std::fs::write(base.join("ggml.dll"), b"x").unwrap();
        assert_eq!(find_library(std::slice::from_ref(&base)), None);
        // renamed by a later release
        let renamed = base.join("cactus-native-2.dll");
        std::fs::write(&renamed, b"x").unwrap();
        assert_eq!(find_library(std::slice::from_ref(&base)), Some(renamed));
        // what the real Windows wheel ships wins over a guess
        let real = base.join("libcactus.dll");
        std::fs::write(&real, b"x").unwrap();
        assert_eq!(find_library(std::slice::from_ref(&base)), Some(real));
    }

    #[test]
    fn decide_reads_calls_confidence_and_suppressed_calls() {
        let f = Fake::with_models(TEXT);
        f.queue_call("fs_list", json!({"path": "notes"}), 0.93);
        let tools = vec![
            json!({"type": "function", "function": {"name": "fs_list", "description": "list", "parameters": {"type": "object"}}}),
        ];
        let d = decide(&f, "date: 2026-11-21", &tools, "list my notes").unwrap();
        assert_eq!(
            d.calls,
            vec![ToolCall {
                name: "fs_list".into(),
                arguments: json!({"path": "notes"})
            }]
        );
        assert_eq!(d.confidence, Some(0.93));
        let seen = f.seen_tools.lock();
        assert_eq!(
            seen[0].1[0]["name"], "fs_list",
            "OpenAI's wrapper is flattened for Needle"
        );
        drop(seen);
        f.tool_replies
            .lock()
            .push_back(json!({"success": false, "error": "grammar failed"}));
        assert_eq!(decide(&f, "", &tools, "x").unwrap_err(), "grammar failed");
        f.tool_replies.lock().push_back(
            json!({"success": true, "function_calls": [], "suppressed_calls": [{"name": "x", "arguments": {}}]}),
        );
        let d = decide(&f, "", &tools, "x").unwrap();
        assert!(d.calls.is_empty() && d.suppressed.len() == 1 && d.confidence.is_none());
    }

    #[test]
    fn archives_are_loaded_once_and_only_when_present() {
        let d = crate::testing::TestDir::new("misty-cactus3");
        let dir = archive_dir(&d);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("needle3.cact"), b"x").unwrap();
        std::fs::write(dir.join("whistle.cact"), b"x").unwrap();
        let f = Fake::with_models(0);
        let r = load_archives(&f, &d);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|x| x.2.is_ok()));
        assert_eq!(f.status().models, TEXT | SPEECH);
        assert!(f.status().has(SPEECH) && !f.status().has(VISION));
        let r = load_archives(&f, &d);
        assert!(r.iter().all(|x| x.2.is_ok()), "a second pass loads nothing twice");
    }

    fn wav16(rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend((36 + samples.len() as u32 * 2).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * 2).to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend((samples.len() as u32 * 2).to_le_bytes());
        for s in samples {
            v.extend(s.to_le_bytes());
        }
        v
    }

    #[test]
    fn audio_is_resampled_to_16k() {
        let samples: Vec<i16> = (0..8000).map(|i| ((i as f32 / 10.0).sin() * 8000.0) as i16).collect();
        let wav = wav16(8000, &samples);
        let pcm = pcm16k(&wav).unwrap();
        assert!((pcm.len() as i64 - 16_000).abs() <= 2, "{}", pcm.len());
        let same = wav16(16_000, &samples);
        assert_eq!(pcm16k(&same).unwrap().len(), 8000);
        assert!(pcm16k(b"not a wav").is_err());
    }
}

//! What a llama.cpp server says about how it placed and runs a model, read from its log.
//!
//! The server prints the details only at verbosity 4 (`-lv 4`, which the hub passes unless the owner's own arguments
//! set a verbosity): the devices and the memory fit, how many layers went to the GPU, every buffer on every device, the
//! KV cache and the recurrent state, the compute graph and how many splits it has, flash attention, the context and
//! the slots. At the default verbosity none of that is printed, and the hub could only guess where the weights were.
//!
//! [`parse`] reads a log into a [`LoadReport`]. [`recent_requests`] reads the timings the server prints after each
//! answer, [`parse_prometheus`] and [`slots_summary`] read what its `/metrics` and `/slots` say, and [`log_header`]
//! writes the lines the hub puts at the top of every log (what it ran, and why).

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Lines kept of one kind of free text (fit lines, warnings, notes).
const KEEP_LINES: usize = 60;

/// A device the server found (`- Vulkan0 : AMD Radeon RX 7900 XTX (24560 MiB, 23743 MiB free)`), the CPU included.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub total_mib: u64,
    pub free_mib: u64,
}

/// The CPU the server runs on.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Cpu {
    pub threads: Option<u64>,
    pub threads_batch: Option<u64>,
    pub hardware_threads: Option<u64>,
    /// Instruction sets the build uses (`AVX2`, `FMA`, ...).
    pub features: Vec<String>,
}

/// One row of the server's memory breakdown before the load (`--fit`): what a device or the host would hold.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct MemoryRow {
    pub name: String,
    pub total_mib: Option<i64>,
    pub free_mib: Option<i64>,
    pub used_mib: i64,
    pub model_mib: i64,
    pub context_mib: i64,
    pub compute_mib: i64,
    pub unaccounted_mib: Option<i64>,
}

/// The server's fit of the load to the memory the devices have free (`--fit on`).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Fit {
    /// Whether the fit succeeded (None: the log has no fit lines).
    pub ok: Option<bool>,
    /// Whether it had to change the load (layers, experts or the context) to make it fit.
    pub changed: Option<bool>,
    /// Whether a line of the fit says it moved tensors (experts, overrides) off the card.
    pub moved_tensors: bool,
    pub projected_mib: Option<u64>,
    pub free_mib: Option<u64>,
    pub leaves_mib: Option<u64>,
    pub target_mib: Option<u64>,
    pub took_s: Option<f64>,
    pub breakdown: Vec<MemoryRow>,
    /// The fit's own lines, in order (what it tried and why).
    pub lines: Vec<String>,
}

/// What the file says the model is (`print_info:` lines and the tensor census).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ModelInfo {
    pub arch: Option<String>,
    pub name: Option<String>,
    pub file_type: Option<String>,
    pub file_size_mib: Option<f64>,
    pub bits_per_weight: Option<f64>,
    pub params: Option<String>,
    pub n_layer: Option<u64>,
    pub n_expert: Option<u64>,
    pub n_expert_used: Option<u64>,
    pub n_ctx_train: Option<u64>,
    pub n_swa: Option<u64>,
    /// A recurrent or state-space part (a hybrid model has attention in some layers only).
    pub recurrent: bool,
    pub n_tensors: Option<u64>,
    pub n_kv: Option<u64>,
    /// Tensors by quantisation type (`q8_0`: 36).
    pub tensor_types: BTreeMap<String, u64>,
    /// Every `print_info:` value (heads, widths, rope, ssm, vocabulary), by name.
    pub info: BTreeMap<String, String>,
}

/// How many layers went to the GPU, as the server counts them.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Layers {
    pub total: u64,
    pub offloaded: u64,
    pub repeating_offloaded: Option<u64>,
    pub output_on_gpu: Option<bool>,
}

/// One buffer the server allocated (`Vulkan0 model buffer size = 312.10 MiB`).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Buffer {
    /// `model`, `kv`, `state`, `compute` or `output`.
    pub kind: String,
    pub device: String,
    /// On the host (CPU, mapped file, pinned host memory) and not on a card.
    pub host: bool,
    pub mib: f64,
}

/// A part of a cache (`K (f16): 24.00 MiB`).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Part {
    pub name: String,
    pub dtype: String,
    pub mib: f64,
}

/// The KV cache or the recurrent state, with what it holds.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Cache {
    /// `kv` or `state`.
    pub kind: String,
    /// A sliding-window cache (None: the log does not say).
    pub swa: Option<bool>,
    pub mib: f64,
    pub cells: Option<u64>,
    pub layers: Option<u64>,
    pub seqs: Option<String>,
    pub parts: Vec<Part>,
}

/// The compute graph the server reserved.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Graph {
    pub nodes: Option<u64>,
    /// Where the graph is cut between backends (for the prompt graph when the server prints both): more than a few
    /// means layers are split between devices.
    pub splits: Option<u64>,
    /// The same for the graph that makes one token (`tg`), when the server prints both.
    pub splits_decode: Option<u64>,
    /// Which graphs the line is about (`pp bs=512, tg bs=1`).
    pub shape: Option<String>,
    pub reserve_ms: Option<f64>,
    pub sched_copies: Option<u64>,
}

/// What the server set up to answer requests.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ServerInfo {
    pub slots: Option<u64>,
    pub n_ctx_slot: Option<u64>,
    pub kv_unified: Option<String>,
    pub prompt_cache_mib: Option<u64>,
    pub checkpoints_max: Option<u64>,
    pub checkpoints_min_spacing: Option<u64>,
    pub thinking: Option<bool>,
    pub speculative: Option<String>,
    /// The server said it was listening.
    pub ready: bool,
    pub notes: Vec<String>,
}

/// How long the stages of the load took, from the log's own clock.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Stages {
    pub fit_s: Option<f64>,
    /// Reading the weights (from disk to the devices: on a hard drive this is most of the load).
    pub tensors_s: Option<f64>,
    /// The file's size over `tensors_s`: about what the disk gave.
    pub tensors_mib_per_s: Option<f64>,
    /// Building the context and reserving the compute graph.
    pub context_s: Option<f64>,
    pub warmup_s: Option<f64>,
    /// From the server's first line to "model loaded".
    pub ready_s: Option<f64>,
}

/// Everything a server's log says about a load.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LoadReport {
    /// The verbosity the server ran at (the load details need 4).
    pub verbosity: Option<u64>,
    /// Whether the log holds the load details (layers and buffers). False at the default verbosity.
    pub detailed: bool,
    pub build: Option<String>,
    pub devices: Vec<Device>,
    pub cpu: Cpu,
    pub fit: Fit,
    pub model: ModelInfo,
    pub layers: Option<Layers>,
    /// `mmap` (the file stays mapped) or `none`/`direct` (read into the devices).
    pub load_mode: Option<String>,
    pub buffers: Vec<Buffer>,
    /// MiB on cards and MiB on the host, over every buffer.
    pub gpu_mib: f64,
    pub host_mib: f64,
    pub caches: Vec<Cache>,
    /// `llama_context:` values (batch sizes, flash attention as asked, rope, sequences).
    pub context: BTreeMap<String, String>,
    /// Whether flash attention is in use after the server resolved `auto`.
    pub flash_attention: Option<bool>,
    pub fused_ops: Vec<String>,
    pub graphs: Vec<Graph>,
    pub server: ServerInfo,
    pub stages: Stages,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    /// Lines worth reading that are not a value (a context smaller than the model's, a cache kind).
    pub notes: Vec<String>,
}

/// The few numbers of a load that fit in a model listing: where the weights went.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LayerSummary {
    pub total: u64,
    pub offloaded: u64,
    pub output_on_gpu: Option<bool>,
    pub gpu_model_mib: f64,
    pub host_model_mib: f64,
    pub kv_mib: f64,
    pub state_mib: f64,
    pub gpu_compute_mib: f64,
    /// Splits of the graph that makes one token: 1 or 2 for a model wholly on one device.
    pub graph_splits: Option<u64>,
    /// Splits of the graph that reads a prompt, when the server prints it apart (a layer split shows here first).
    pub graph_splits_prompt: Option<u64>,
    pub flash_attention: Option<bool>,
    /// The fit moved tensors (experts) off the card although every layer is on it.
    pub tensors_moved: bool,
}

impl LoadReport {
    /// The summary, when the log says how many layers were offloaded.
    pub fn summary(&self) -> Option<LayerSummary> {
        let l = self.layers.as_ref()?;
        let sum = |kind: &str, host: bool| -> f64 {
            self.buffers
                .iter()
                .filter(|b| b.kind == kind && b.host == host)
                .map(|b| b.mib)
                .sum()
        };
        // two decimals; the `+ 0.0` turns the -0.0 an empty sum gives into 0
        let r = |x: f64| (x * 100.0).round() / 100.0 + 0.0;
        Some(LayerSummary {
            total: l.total,
            offloaded: l.offloaded,
            output_on_gpu: l.output_on_gpu,
            gpu_model_mib: r(sum("model", false)),
            host_model_mib: r(sum("model", true)),
            kv_mib: r(sum("kv", false) + sum("kv", true)),
            state_mib: r(sum("state", false) + sum("state", true)),
            gpu_compute_mib: r(sum("compute", false)),
            graph_splits: self.graphs.first().and_then(|g| g.splits_decode.or(g.splits)),
            graph_splits_prompt: self.graphs.first().and_then(|g| g.splits_decode.and(g.splits)),
            flash_attention: self.flash_attention,
            tensors_moved: self.fit.moved_tensors,
        })
    }

    /// The report as JSON without the empty fields.
    pub fn to_json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut().filter(|_| !self.detailed) {
            // nothing of this was read from the log: leave out the defaults (a false flag, a sum of no buffers)
            for k in ["fit", "gpu_mib", "host_mib"] {
                o.remove(k);
            }
            if self.model.arch.is_none() {
                o.remove("model");
            }
        }
        prune(v)
    }
}

/// Where the weights are, from what the server says: "gpu", "partial" or "cpu". `overrides` says the hub's own
/// arguments move tensors off the card (`--cpu-moe`, `--n-cpu-moe`, `-ot`).
pub fn placement_from_layers(l: &LayerSummary, overrides: bool) -> Option<&'static str> {
    if l.total == 0 {
        return None;
    }
    if l.offloaded == 0 {
        return Some("cpu");
    }
    if l.offloaded < l.total || l.output_on_gpu == Some(false) || overrides || l.tensors_moved {
        return Some("partial");
    }
    Some("gpu")
}

/// Whether these llama-server arguments already say a verbosity.
pub fn sets_verbosity(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-lv" | "--verbosity" | "--log-verbosity" | "-v" | "--verbose" | "--log-verbose"
        ) || a.starts_with("--verbosity=")
            || a.starts_with("--log-verbosity=")
    })
}

/// Whether these llama-server arguments move tensors off the card on their own (experts to the CPU, overrides).
pub fn moves_tensors(args: &[String]) -> bool {
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cpu-moe" | "-cmoe" => return true,
            "--n-cpu-moe" | "-ncmoe" => {
                if it.peek().and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n > 0) {
                    return true;
                }
            }
            "-ot" | "--override-tensor" => return true,
            _ => {}
        }
    }
    false
}

// ---- reading a log -------------------------------------------------------------------------------------------------

/// Reads a server log. Lines it does not know are skipped, so a newer server that prints more still parses.
pub fn parse(text: &str) -> LoadReport {
    let mut p = Parser::default();
    for line in text.lines() {
        p.feed(line);
    }
    p.finish()
}

/// `0.01.196.744 I load_tensors: ...`: minutes, seconds, milliseconds and microseconds since the server started, the
/// level, the message.
fn split_line(line: &str) -> Option<(f64, char, &str)> {
    let (stamp, rest) = line.split_once(' ')?;
    let mut parts = stamp.split('.');
    let mut next = || parts.next().and_then(|x| x.parse::<f64>().ok());
    let (m, s, ms, us) = (next()?, next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    let rest = rest.trim_start();
    let level = rest.chars().next()?;
    let msg = rest[level.len_utf8()..].trim_start();
    Some((m * 60.0 + s + ms / 1e3 + us / 1e6, level, msg))
}

/// The message without the module tag (`srv`, `cmn`, `slot`, `spec`) some lines carry.
fn body(msg: &str) -> &str {
    for tag in ["srv", "cmn", "slot", "spec"] {
        if let Some(rest) = msg.strip_prefix(tag) {
            if rest.starts_with(' ') {
                return rest.trim_start();
            }
        }
    }
    msg
}

/// The text after `marker`, trimmed.
fn after<'a>(s: &'a str, marker: &str) -> Option<&'a str> {
    s.find(marker).map(|i| s[i + marker.len()..].trim())
}

/// The number that starts `s` (after spaces).
fn take_number(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s[..end].parse().ok()
}

/// The number after `marker`.
fn num_after(s: &str, marker: &str) -> Option<f64> {
    s.find(marker).and_then(|i| take_number(&s[i + marker.len()..]))
}

fn uint_after(s: &str, marker: &str) -> Option<u64> {
    num_after(s, marker).filter(|n| *n >= 0.0).map(|n| n as u64)
}

/// The integer after `marker` and, when a `/` follows it, the one after that (`splits = 257 / 2`).
fn pair_after(s: &str, marker: &str) -> (Option<u64>, Option<u64>) {
    let Some(i) = s.find(marker) else {
        return (None, None);
    };
    let rest = s[i + marker.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let first = rest[..end].parse().ok();
    let second = rest[end..]
        .trim_start()
        .strip_prefix('/')
        .and_then(take_number)
        .map(|n| n as u64);
    (first, second)
}

/// The integer right before `marker` (`4096 cells`).
fn uint_before(s: &str, marker: &str) -> Option<u64> {
    let i = s.find(marker)?;
    let head = s[..i].trim_end();
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, _)| i)?;
    head[start..].parse().ok()
}

fn is_host(device: &str) -> bool {
    let u = device.to_ascii_uppercase();
    u.starts_with("CPU") || u.contains("HOST") || u.contains("MAPPED")
}

fn push_capped(list: &mut Vec<String>, line: &str) {
    if list.len() < KEEP_LINES {
        list.push(line.to_string());
    }
}

#[derive(Default)]
struct Parser {
    r: LoadReport,
    t_first: Option<f64>,
    t_fit0: Option<f64>,
    t_fit1: Option<f64>,
    t_tensors0: Option<f64>,
    t_tensors1: Option<f64>,
    t_ctx0: Option<f64>,
    t_ctx1: Option<f64>,
    t_warm0: Option<f64>,
    t_ready: Option<f64>,
    pending_swa: Option<bool>,
    repeating: Option<u64>,
    output_gpu: Option<bool>,
    offloaded: Option<(u64, u64)>,
}

impl Parser {
    fn feed(&mut self, line: &str) {
        let Some((t, level, msg)) = split_line(line) else {
            return;
        };
        if self.t_first.is_none() {
            self.t_first = Some(t);
        }
        let b = body(msg);
        if matches!(level, 'W' | 'E') {
            self.r.warning_count += 1;
            push_capped(&mut self.r.warnings, b);
        }
        if let Some((head, tail)) = b.split_once(" buffer size =") {
            self.buffer(t, head, tail);
            return;
        }
        if b.contains("common_fit") || b.contains("common_params_fit") || b.contains("common_memory_breakdown_print") {
            self.fit_line(t, b);
            return;
        }
        // the function's name is cut to the width of its column (`common_param:`) before the message, which names it again
        let named = b.strip_prefix("common_param:").map_or(b, str::trim_start);
        if let Some(rest) = named.strip_prefix("common_params_print_info:") {
            let rest = rest.trim();
            if rest.starts_with("build") {
                self.r.build = Some(rest.to_string());
            } else if let Some(v) = rest.strip_prefix("verbosity =") {
                self.r.verbosity = take_number(v).map(|n| n as u64);
            }
            return;
        }
        if let Some(rest) = b.strip_prefix("common_param:") {
            self.param_line(rest.trim());
            return;
        }
        if let Some(rest) = b.strip_prefix("llama_model_loader:") {
            self.loader_line(rest.trim());
            return;
        }
        if let Some(rest) = b.strip_prefix("print_info:") {
            if let Some((k, v)) = rest.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if !k.is_empty() && !k.ends_with(" token") && self.r.model.info.len() < 160 {
                    self.r.model.info.insert(k.to_string(), v.to_string());
                }
            }
            return;
        }
        if b.starts_with("load_tensors:") {
            self.tensors_line(t, b);
            return;
        }
        if let Some(rest) = b.strip_prefix("llama_context:") {
            self.context_line(t, rest.trim());
            return;
        }
        if b.starts_with("llama_kv_cache") || b.starts_with("llama_memory_recurrent") {
            self.cache_line(b);
            return;
        }
        if b.starts_with("sched_reserve:") {
            self.sched_line(t, b);
            return;
        }
        if let Some(rest) = b.strip_prefix("resolve_fused_ops:") {
            let rest = rest.trim();
            if rest.contains("Flash Attention") {
                self.r.flash_attention = Some(rest.contains("enabled") && !rest.contains("disabled"));
            } else if let Some(name) = rest
                .strip_prefix("fused ")
                .and_then(|n| n.strip_suffix(" enabled"))
                .filter(|_| !rest.contains("resolving"))
            {
                if self.r.fused_ops.len() < KEEP_LINES {
                    self.r.fused_ops.push(name.to_string());
                }
            }
            return;
        }
        self.server_line(t, b);
    }

    fn buffer(&mut self, t: f64, head: &str, tail: &str) {
        let words: Vec<&str> = head.split_whitespace().collect();
        if words.len() < 2 {
            return;
        }
        let Some(mib) = take_number(tail) else {
            return;
        };
        let device = words[words.len() - 2].to_string();
        let kind = match words[words.len() - 1] {
            "model" => "model".to_string(),
            "KV" => "kv".to_string(),
            "RS" => "state".to_string(),
            "compute" => "compute".to_string(),
            "output" => "output".to_string(),
            other => other.to_ascii_lowercase(),
        };
        if head.contains("load_tensors") {
            self.t_tensors1 = Some(t);
        }
        if self.r.buffers.len() < 96 {
            self.r.buffers.push(Buffer {
                host: is_host(&device),
                kind,
                device,
                mib,
            });
        }
    }

    fn fit_line(&mut self, t: f64, b: &str) {
        let text = b.split_once(':').map(|(_, m)| m.trim()).unwrap_or(b);
        let f = &mut self.r.fit;
        if text.starts_with('|') {
            if let Some(row) = memory_row(text) {
                f.breakdown.push(row);
            }
            push_capped(&mut f.lines, text);
            return;
        }
        push_capped(&mut f.lines, text);
        if text.starts_with("fitting params to device memory") {
            self.t_fit0 = Some(t);
        }
        if text.contains("projected to use") {
            f.projected_mib = uint_after(text, "projected to use");
            f.free_mib = uint_after(text, "vs.");
        }
        if text.contains("will leave") {
            f.leaves_mib = uint_after(text, "will leave");
            f.target_mib = uint_after(text, ">=");
        }
        if text.contains("no changes needed") {
            f.changed = Some(false);
        }
        if text.contains("successfully fit") {
            f.ok = Some(true);
        } else if ["failed", "unable", "could not", "cannot"]
            .iter()
            .any(|w| text.contains(w))
        {
            f.ok = Some(false);
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("overrid") || lower.contains("expert") || lower.contains("cpu-moe") {
            f.moved_tensors = true;
        }
        if text.contains("took") && text.contains("seconds") {
            f.took_s = num_after(text, "took");
            self.t_fit1 = Some(t);
        }
    }

    fn param_line(&mut self, rest: &str) {
        if let Some(sys) = rest.strip_prefix("system_info:") {
            let c = &mut self.r.cpu;
            c.threads = uint_after(sys, "n_threads =");
            c.threads_batch = uint_after(sys, "n_threads_batch =");
            c.hardware_threads = uint_after(sys, ") /");
            for seg in sys.split('|').skip(1) {
                let seg = seg.trim().trim_start_matches("CPU :").trim();
                if let Some((name, v)) = seg.split_once(" = ") {
                    if v.trim() == "1" {
                        c.features.push(name.trim().to_string());
                    }
                }
            }
            return;
        }
        let Some(dev) = rest.strip_prefix("- ") else {
            return;
        };
        let Some((name, tail)) = dev.split_once(" : ") else {
            return;
        };
        let Some(open) = tail.rfind('(') else {
            return;
        };
        let inside = tail[open + 1..].trim_end_matches(')');
        let (Some(total), Some(free)) = (uint_after(inside, ""), uint_after(inside, ", ")) else {
            return;
        };
        self.r.devices.push(Device {
            name: name.trim().to_string(),
            description: tail[..open].trim().to_string(),
            total_mib: total,
            free_mib: free,
        });
    }

    fn loader_line(&mut self, rest: &str) {
        let m = &mut self.r.model;
        if rest.starts_with("loaded meta data") {
            m.n_kv = uint_after(rest, "with");
            m.n_tensors = uint_before(rest, " tensors");
        } else if let Some(t) = rest.strip_prefix("- type") {
            if let Some((name, count)) = t.split_once(':') {
                if let Some(n) = uint_before(count, " tensors") {
                    m.tensor_types.insert(name.trim().to_string(), n);
                }
            }
        }
    }

    fn tensors_line(&mut self, t: f64, b: &str) {
        if b.contains("loading model tensors") {
            self.t_tensors0 = Some(t);
            self.r.load_mode = after(b, "load_mode =").map(|m| m.trim_end_matches(')').trim().to_string());
        } else if b.contains("offloading output layer") {
            self.output_gpu = Some(!b.contains("to CPU"));
        } else if b.contains("repeating layers") {
            self.repeating = uint_before(b, " repeating layers");
        } else if let Some(i) = b.find("offloaded").filter(|_| b.contains("layers")) {
            // `offloaded 10/25 layers to GPU`
            let rest = &b[i + "offloaded".len()..];
            let offloaded = take_number(rest).unwrap_or(0.0) as u64;
            let total = rest.split_once('/').and_then(|(_, r)| take_number(r)).unwrap_or(0.0) as u64;
            self.offloaded = Some((offloaded, total));
            self.t_tensors1 = Some(t);
        }
    }

    fn context_line(&mut self, t: f64, rest: &str) {
        if rest.starts_with("constructing") {
            self.t_ctx0 = Some(t);
            return;
        }
        match rest.split_once('=') {
            Some((k, v)) if !k.trim().contains(' ') && !k.trim().is_empty() => {
                self.r.context.insert(k.trim().to_string(), v.trim().to_string());
            }
            _ => push_capped(&mut self.r.notes, rest),
        }
    }

    fn cache_line(&mut self, b: &str) {
        if b.contains("creating") && b.contains("KV cache") {
            self.pending_swa = Some(!b.contains("non-SWA"));
            push_capped(&mut self.r.notes, b);
            return;
        }
        let kind = if b.starts_with("llama_memory_recurrent") {
            "state"
        } else {
            "kv"
        };
        let Some(i) = b.find(": size =") else {
            return;
        };
        let line = &b[i + 2..];
        let Some(mib) = num_after(line, "size =") else {
            return;
        };
        let mut c = Cache {
            kind: kind.to_string(),
            swa: if kind == "kv" { self.pending_swa.take() } else { None },
            mib,
            cells: uint_before(line, " cells"),
            layers: uint_before(line, " layers"),
            ..Default::default()
        };
        if let Some(j) = line.find(" seqs") {
            c.seqs = line[..j].split_whitespace().last().map(str::to_string);
        }
        if let Some(j) = line.find("), ") {
            for seg in line[j + 3..].split(", ") {
                let (Some(open), Some(close)) = (seg.find('('), seg.find(')')) else {
                    continue;
                };
                if let Some(mib) = num_after(seg, "):") {
                    c.parts.push(Part {
                        name: seg[..open].trim().to_string(),
                        dtype: seg[open + 1..close].to_string(),
                        mib,
                    });
                }
            }
        }
        if self.r.caches.len() < 8 {
            self.r.caches.push(c);
        }
    }

    fn sched_line(&mut self, t: f64, b: &str) {
        if b.contains("graph") && b.contains("nodes =") {
            if self.r.graphs.len() < 6 {
                // `graph: nodes = 1431, splits = 2, ...` or, when the prompt and the decode graph differ,
                // `graph (pp bs=512, tg bs=1): nodes = 1431 / 1431, splits = 257 / 2, ...`
                let shape = b.find("graph (").and_then(|i| {
                    let rest = &b[i + "graph (".len()..];
                    rest.find(')').map(|j| rest[..j].to_string())
                });
                self.r.graphs.push(Graph {
                    nodes: pair_after(b, "nodes =").0,
                    splits: pair_after(b, "splits =").0,
                    splits_decode: pair_after(b, "splits =").1,
                    shape,
                    ..Default::default()
                });
            }
        } else if b.contains("reserve took") {
            self.t_ctx1 = Some(t);
            if let Some(g) = self.r.graphs.last_mut() {
                g.reserve_ms = num_after(b, "reserve took");
                g.sched_copies = uint_after(b, "sched copies =");
            }
        }
    }

    fn server_line(&mut self, t: f64, b: &str) {
        let s = &mut self.r.server;
        if b.contains("initializing, n_slots") {
            s.slots = uint_after(b, "n_slots =");
            s.n_ctx_slot = uint_after(b, "n_ctx_slot =");
            s.kv_unified = after(b, "kv_unified =").map(|v| v.trim_matches('\'').to_string());
        } else if b.contains("prompt cache is enabled") {
            s.prompt_cache_mib = uint_after(b, "size limit:");
        } else if b.contains("context checkpoints enabled") {
            s.checkpoints_max = uint_after(b, "max =");
            s.checkpoints_min_spacing = uint_after(b, "min spacing =");
        } else if b.contains("chat template, thinking") {
            s.thinking = uint_after(b, "thinking =").map(|n| n != 0);
        } else if b.contains("no implementations specified for speculative decoding") {
            s.speculative = Some("none".to_string());
        } else if b.contains("speculative decoding will use") {
            s.speculative = Some(after(b, "will use").unwrap_or("checkpoints").to_string());
        } else if b.contains("does not support partial sequence removal") {
            push_capped(&mut s.notes, b);
        } else if b.contains("warming up the model") {
            self.t_warm0 = Some(t);
        } else if b.contains("model loaded") {
            self.t_ready = Some(t);
        } else if b.contains("listening on") {
            s.ready = true;
        }
    }

    fn finish(mut self) -> LoadReport {
        let r = &mut self.r;
        r.detailed = self.offloaded.is_some() || !r.buffers.is_empty();
        if let Some((off, total)) = self.offloaded {
            r.layers = Some(Layers {
                total,
                offloaded: off,
                repeating_offloaded: self.repeating,
                output_on_gpu: self.output_gpu,
            });
        }
        let total = |host: bool| r.buffers.iter().filter(|b| b.host == host).map(|b| b.mib).sum::<f64>();
        (r.gpu_mib, r.host_mib) = (
            (total(false) * 100.0).round() / 100.0 + 0.0,
            (total(true) * 100.0).round() / 100.0 + 0.0,
        );
        if r.fit.ok == Some(true) && r.fit.changed.is_none() {
            r.fit.changed = Some(true);
        }
        let m = &mut r.model;
        let get = |m: &ModelInfo, k: &str| m.info.get(k).cloned();
        let num = |m: &ModelInfo, k: &str| {
            m.info
                .get(k)
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        };
        m.arch = get(m, "arch");
        m.name = get(m, "general.name");
        m.file_type = get(m, "file type");
        m.params = get(m, "model params");
        m.n_layer = num(m, "n_layer");
        m.n_expert = num(m, "n_expert");
        m.n_expert_used = num(m, "n_expert_used");
        m.n_ctx_train = num(m, "n_ctx_train");
        m.n_swa = num(m, "n_swa");
        m.recurrent = num(m, "ssm_d_state").is_some_and(|n| n > 0);
        if let Some(size) = m.info.get("file size") {
            // the server prints the size in the unit that suits it: `312.10 MiB`, `10.95 GiB`
            let scale = if size.contains("GiB") {
                1024.0
            } else if size.contains("KiB") {
                1.0 / 1024.0
            } else {
                1.0
            };
            m.file_size_mib = take_number(size).map(|n| n * scale);
            m.bits_per_weight = after(size, "(").and_then(take_number);
        }
        let span = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) if b >= a => Some(((b - a) * 100.0).round() / 100.0),
            _ => None,
        };
        let st = &mut r.stages;
        st.fit_s = r.fit.took_s.or_else(|| span(self.t_fit0, self.t_fit1));
        // the weights are read from the file after the buffers are allocated and before the context is built (the log
        // prints the buffers first): on a hard drive that is where a load spends its minutes
        st.tensors_s = span(self.t_tensors0, self.t_ctx0.or(self.t_tensors1));
        st.tensors_mib_per_s = match (st.tensors_s, r.model.file_size_mib) {
            (Some(s), Some(mib)) if s >= 1.0 => Some((mib / s * 10.0).round() / 10.0),
            _ => None,
        };
        st.context_s = span(self.t_ctx0, self.t_ctx1);
        st.warmup_s = span(self.t_warm0, self.t_ready);
        st.ready_s = span(self.t_first, self.t_ready);
        if !r.detailed && matches!(r.verbosity, Some(v) if v < 4) {
            r.notes.push(format!(
                "the server ran at verbosity {}: it prints the layers, buffers and fit only at 4 (-lv 4)",
                r.verbosity.unwrap_or(3)
            ));
        }
        self.r
    }
}

/// `|   - Vulkan0 (RX 7900 XTX) | 24560 = 23743 + ( 416 =   312 +      67 +      37) +         399 |`
fn memory_row(text: &str) -> Option<MemoryRow> {
    let inner = text.trim().trim_matches('|');
    let (name, numbers) = inner.split_once('|')?;
    let name = name.trim().trim_start_matches("- ").trim().to_string();
    if name.is_empty() || name.contains("memory breakdown") {
        return None;
    }
    let nums: Vec<i64> = numbers
        .replace(['=', '+', '(', ')'], " ")
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    match nums[..] {
        [total, free, used, model, context, compute, unaccounted] => Some(MemoryRow {
            name,
            total_mib: Some(total),
            free_mib: Some(free),
            used_mib: used,
            model_mib: model,
            context_mib: context,
            compute_mib: compute,
            unaccounted_mib: Some(unaccounted),
        }),
        [used, model, context, compute] => Some(MemoryRow {
            name,
            total_mib: None,
            free_mib: None,
            used_mib: used,
            model_mib: model,
            context_mib: context,
            compute_mib: compute,
            unaccounted_mib: None,
        }),
        _ => None,
    }
}

// ---- requests ------------------------------------------------------------------------------------------------------

/// What the server printed after one answer.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RequestTiming {
    pub task: u64,
    pub prompt_tokens: Option<u64>,
    pub prompt_ms: Option<f64>,
    pub prompt_tokens_per_s: Option<f64>,
    pub generated_tokens: Option<u64>,
    pub generation_ms: Option<f64>,
    pub generation_tokens_per_s: Option<f64>,
    pub total_ms: Option<f64>,
    pub total_tokens: Option<u64>,
    pub graphs_reused: Option<u64>,
    /// Graph re-allocations: of the same size, and of a changed size.
    pub graph_reallocs: Option<(u64, u64)>,
    /// Tokens the slot held when it was released.
    pub context_tokens: Option<u64>,
    pub truncated: Option<bool>,
}

/// The last `n` requests the log shows, newest last. Only `print_timing` blocks that reached the total count.
pub fn recent_requests(text: &str, n: usize) -> Vec<RequestTiming> {
    let mut done: Vec<RequestTiming> = vec![];
    let mut cur: Option<RequestTiming> = None;
    for line in text.lines() {
        let Some((_, _, msg)) = split_line(line) else {
            continue;
        };
        let b = body(msg);
        let (is_timing, is_release) = (b.starts_with("print_timing:"), b.starts_with("release:"));
        if !is_timing && !is_release {
            continue;
        }
        let parts: Vec<&str> = b.split('|').collect();
        if parts.len() < 3 {
            continue;
        }
        let Some(task) = uint_after(parts[1], "task") else {
            continue;
        };
        let what = parts[2].trim();
        if is_release {
            if let Some(i) = done.iter().rposition(|d| d.task == task) {
                done[i].context_tokens = uint_after(what, "n_tokens =");
                done[i].truncated = uint_after(what, "truncated =").map(|v| v != 0);
            }
            continue;
        }
        if cur.as_ref().is_some_and(|c| c.task != task) {
            cur = None;
        }
        let c = cur.get_or_insert_with(|| RequestTiming {
            task,
            ..Default::default()
        });
        if what.starts_with("prompt eval time") {
            c.prompt_ms = num_after(what, "=");
            c.prompt_tokens = uint_before(what, " tokens (");
            c.prompt_tokens_per_s = num_after(what, "ms per token,");
        } else if what.starts_with("eval time") {
            c.generation_ms = num_after(what, "=");
            c.generated_tokens = uint_before(what, " tokens (");
            c.generation_tokens_per_s = num_after(what, "ms per token,");
        } else if what.starts_with("total time") {
            c.total_ms = num_after(what, "=");
            c.total_tokens = uint_before(what, " tokens");
        } else if what.starts_with("graphs reused") {
            c.graphs_reused = uint_after(what, "=");
        } else if what.starts_with("graph reallocs") {
            let same = uint_after(what, "=");
            let changed = what.split_once('/').and_then(|(_, r)| take_number(r)).map(|n| n as u64);
            c.graph_reallocs = same.zip(changed);
            // the last line of a block
            if let Some(c) = cur.take() {
                done.push(c);
            }
        }
    }
    let skip = done.len().saturating_sub(n);
    done.split_off(skip)
}

/// A Prometheus text page (`llamacpp:tokens_predicted_total 800`) as name and value, the `llamacpp:` prefix dropped.
pub fn parse_prometheus(text: &str) -> BTreeMap<String, f64> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (name, v) = l.trim().rsplit_once(' ')?;
            Some((name.trim().trim_start_matches("llamacpp:").to_string(), v.parse().ok()?))
        })
        .collect()
}

/// The fields of `/slots` worth showing (the sampler settings of each slot are left out).
pub fn slots_summary(slots: &Value) -> Vec<Value> {
    slots
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    let mut o = serde_json::Map::new();
                    for k in [
                        "id",
                        "n_ctx",
                        "is_processing",
                        "id_task",
                        "n_prompt_tokens",
                        "n_prompt_tokens_processed",
                        "n_prompt_tokens_cache",
                        "speculative",
                    ] {
                        if let Some(v) = s.get(k) {
                            o.insert(k.to_string(), v.clone());
                        }
                    }
                    Value::Object(o)
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- the hub's own lines -------------------------------------------------------------------------------------------

/// `2026-11-25T12:49:03Z` for seconds since the epoch.
pub fn iso_utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The lines the hub writes at the top of a model's log before the server starts: what it runs and why, so a log
/// says how the load was made even when the server dies before it says anything. They start with `# mm`.
pub fn log_header(
    version: &str,
    at_s: u64,
    exe: &str,
    args: &[String],
    env: &[(String, String)],
    attempt: &str,
) -> String {
    let mut s = format!("# mm {version} {}: {attempt}\n# mm server: {exe}\n", iso_utc(at_s));
    s.push_str(&format!("# mm args: {}\n", args.join(" ")));
    for (k, v) in env {
        s.push_str(&format!("# mm env: {k}={v}\n"));
    }
    s
}

/// `v` without nulls, empty arrays and empty objects, at any depth.
pub fn prune(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (k, prune(v)))
                .filter(|(_, v)| !is_empty(v))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(prune).collect()),
        other => other,
    }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// A short line for a log or a status page: where the weights are.
pub fn describe_layers(l: &LayerSummary) -> String {
    format!(
        "{}/{} layers on the GPU, weights {:.0} MiB on cards and {:.0} MiB on the host, KV {:.0} MiB, state {:.0} MiB, graph splits {}",
        l.offloaded,
        l.total,
        l.gpu_model_mib,
        l.host_model_mib,
        l.kv_mib,
        l.state_mib,
        l.graph_splits.map_or("?".to_string(), |s| s.to_string())
    )
}

/// One request as JSON for the telemetry answer.
pub fn timing_json(t: &RequestTiming) -> Value {
    prune(json!({
        "task": t.task, "prompt_tokens": t.prompt_tokens, "prompt_ms": t.prompt_ms,
        "prompt_tokens_per_s": t.prompt_tokens_per_s, "generated_tokens": t.generated_tokens,
        "generation_ms": t.generation_ms, "generation_tokens_per_s": t.generation_tokens_per_s,
        "total_ms": t.total_ms, "graphs_reused": t.graphs_reused,
        "graph_reallocs": t.graph_reallocs.map(|(a, b)| json!({"same_size": a, "changed_size": b})),
        "context_tokens": t.context_tokens, "truncated": t.truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Logs of real servers (llama.cpp b12726, `-lv 4`) with the metadata dump trimmed: a 0.8B hybrid model wholly on
    // the GPU, with 10 of its 25 layers on it, and on the CPU only; gemma-4-31B, which has a sliding-window cache; and
    // the same server at the default verbosity.
    const GPU: &str = include_str!("../tests/fixtures/llama/small-gpu.txt");
    const PARTIAL: &str = include_str!("../tests/fixtures/llama/small-partial.txt");
    const CPU: &str = include_str!("../tests/fixtures/llama/small-cpu.txt");
    const GEMMA: &str = include_str!("../tests/fixtures/llama/gemma.txt");
    const DEFAULT: &str = include_str!("../tests/fixtures/llama/default-verbosity.txt");

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 0.011)
    }

    #[test]
    fn a_model_wholly_on_the_gpu_is_read_in_full() {
        let r = parse(GPU);
        assert_eq!(r.verbosity, Some(4));
        assert!(r.detailed);
        assert!(r.build.as_deref().unwrap().contains("MSVC"));
        assert_eq!(r.devices.len(), 2);
        assert_eq!(
            r.devices[0],
            Device {
                name: "Vulkan0".into(),
                description: "AMD Radeon RX 7900 XTX".into(),
                total_mib: 24560,
                free_mib: 23743
            }
        );
        assert_eq!(r.devices[1].name, "CPU");
        assert_eq!(
            (r.cpu.threads, r.cpu.threads_batch, r.cpu.hardware_threads),
            (Some(12), Some(12), Some(24))
        );
        assert!(r.cpu.features.iter().any(|f| f == "AVX2") && r.cpu.features.iter().any(|f| f == "FMA"));

        // the fit
        let f = &r.fit;
        assert_eq!((f.ok, f.changed, f.moved_tensors), (Some(true), Some(false), false));
        assert_eq!(
            (f.projected_mib, f.free_mib, f.leaves_mib, f.target_mib),
            (Some(416), Some(23743), Some(23327), Some(768))
        );
        assert!(close(f.took_s, 0.60));
        assert_eq!(f.breakdown.len(), 2);
        assert_eq!(
            (
                f.breakdown[0].name.as_str(),
                f.breakdown[0].used_mib,
                f.breakdown[0].model_mib
            ),
            ("Vulkan0 (RX 7900 XTX)", 416, 312)
        );
        assert_eq!(
            (
                f.breakdown[0].context_mib,
                f.breakdown[0].compute_mib,
                f.breakdown[0].unaccounted_mib
            ),
            (67, 37, Some(399))
        );
        assert_eq!(
            (
                f.breakdown[1].name.as_str(),
                f.breakdown[1].total_mib,
                f.breakdown[1].used_mib
            ),
            ("Host", None, 178)
        );

        // what the file is
        let m = &r.model;
        assert_eq!(
            (m.arch.as_deref(), m.n_layer, m.n_expert),
            (Some("qwen35"), Some(24), Some(0))
        );
        assert_eq!(m.file_type.as_deref(), Some("IQ2_XXS - 2.0625 bpw"));
        assert!(close(m.file_size_mib, 312.10) && close(m.bits_per_weight, 3.48));
        assert_eq!(
            (m.params.as_deref(), m.n_tensors, m.n_kv),
            (Some("752.39 M"), Some(320), Some(46))
        );
        assert_eq!((m.tensor_types["q8_0"], m.tensor_types["iq2_xxs"]), (36, 95));
        assert!(m.recurrent, "a hybrid model keeps recurrent state");
        assert_eq!(m.info["n_head_kv"], "2");

        // the layers and the buffers
        assert_eq!(
            r.layers,
            Some(Layers {
                total: 25,
                offloaded: 25,
                repeating_offloaded: Some(23),
                output_on_gpu: Some(true)
            })
        );
        assert_eq!(r.load_mode.as_deref(), Some("mmap"));
        let model_on_gpu = r.buffers.iter().find(|b| b.kind == "model" && !b.host).unwrap();
        assert_eq!((model_on_gpu.device.as_str(), model_on_gpu.mib), ("Vulkan0", 312.10));
        let mapped = r.buffers.iter().find(|b| b.kind == "model" && b.host).unwrap();
        assert_eq!((mapped.device.as_str(), mapped.mib), ("CPU_Mapped", 166.72));
        // on the card: weights, KV cache, recurrent state, compute buffer; on the host: the mapped weights, the pinned
        // output and compute buffers
        assert!(close(Some(r.gpu_mib), 312.10 + 48.0 + 19.27 + 37.02));
        assert!(close(Some(r.host_mib), 166.72 + 0.95 + 12.02));

        // the caches
        assert_eq!(r.caches.len(), 2);
        let kv = &r.caches[0];
        assert_eq!(
            (kv.kind.as_str(), kv.mib, kv.cells, kv.layers),
            ("kv", 48.0, Some(4096), Some(6))
        );
        assert_eq!(kv.seqs.as_deref(), Some("1/1"));
        assert_eq!(
            kv.parts,
            vec![
                Part {
                    name: "K".into(),
                    dtype: "f16".into(),
                    mib: 24.0
                },
                Part {
                    name: "V".into(),
                    dtype: "f16".into(),
                    mib: 24.0
                },
            ]
        );
        let state = &r.caches[1];
        assert_eq!(
            (state.kind.as_str(), state.cells, state.layers),
            ("state", Some(1), Some(24))
        );
        assert_eq!(
            state.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["R", "S", "P"]
        );
        assert_eq!(state.parts[1].mib, 18.0);

        // the context, the graph and the server
        assert_eq!(r.context["n_ctx"], "4096");
        assert_eq!(
            (r.context["n_batch"].as_str(), r.context["n_ubatch"].as_str()),
            ("2048", "512")
        );
        assert_eq!(r.context["flash_attn"], "auto");
        assert_eq!(r.flash_attention, Some(true));
        assert_eq!(r.graphs.len(), 1);
        assert_eq!(
            (r.graphs[0].nodes, r.graphs[0].splits, r.graphs[0].splits_decode),
            (Some(1431), Some(2), None)
        );
        assert!(close(r.graphs[0].reserve_ms, 56.38));
        let s = &r.server;
        assert_eq!(
            (s.slots, s.n_ctx_slot, s.prompt_cache_mib),
            (Some(1), Some(4096), Some(8192))
        );
        assert_eq!(
            (s.checkpoints_max, s.checkpoints_min_spacing, s.thinking),
            (Some(32), Some(8192), Some(true))
        );
        assert!(s.ready);

        // the stages
        assert!(close(r.stages.fit_s, 0.60));
        assert!(r.stages.ready_s.is_some_and(|s| s > 1.0 && s < 3.0));
        assert!(r.stages.tensors_s.is_some_and(|s| s < 1.0));
        assert!(r.stages.context_s.is_some());
        assert!(r.stages.warmup_s.is_some());
        assert!(
            r.warning_count >= 1 && !r.warnings.is_empty(),
            "the server warns about its API key"
        );
    }

    #[test]
    fn the_summary_says_where_the_weights_went() {
        let s = parse(GPU).summary().unwrap();
        assert_eq!((s.total, s.offloaded, s.output_on_gpu), (25, 25, Some(true)));
        assert_eq!(
            (s.gpu_model_mib, s.host_model_mib, s.kv_mib, s.state_mib),
            (312.10, 166.72, 48.0, 19.27)
        );
        assert_eq!(
            (s.graph_splits, s.graph_splits_prompt, s.flash_attention),
            (Some(2), None, Some(true))
        );
        assert_eq!(placement_from_layers(&s, false), Some("gpu"));
        // arguments that move the experts to the CPU make a load partial although every layer is on the card
        assert_eq!(placement_from_layers(&s, true), Some("partial"));
        assert!(describe_layers(&s).starts_with("25/25 layers on the GPU"));
    }

    #[test]
    fn some_layers_on_the_gpu_is_partial_and_the_prompt_graph_shows_the_split() {
        let r = parse(PARTIAL);
        assert_eq!(
            r.layers,
            Some(Layers {
                total: 25,
                offloaded: 10,
                repeating_offloaded: Some(9),
                output_on_gpu: Some(true)
            })
        );
        let g = &r.graphs[0];
        assert_eq!((g.nodes, g.splits, g.splits_decode), (Some(1431), Some(257), Some(2)));
        assert_eq!(g.shape.as_deref(), Some("pp bs=512, tg bs=1"));
        let s = r.summary().unwrap();
        assert_eq!((s.graph_splits, s.graph_splits_prompt), (Some(2), Some(257)));
        // the cache and the state are split too, and each side is counted
        // (the state's two halves, 12.84 and 6.42, add up to the 19.27 the server prints, less a rounding)
        assert_eq!(s.kv_mib, 48.0);
        assert!((s.state_mib - 19.27).abs() < 0.02, "{}", s.state_mib);
        assert!(r.buffers.iter().any(|b| b.kind == "kv" && b.host && b.device == "CPU"));
        assert_eq!((s.gpu_model_mib, s.host_model_mib), (219.79, 259.03));
        assert_eq!(placement_from_layers(&s, false), Some("partial"));
    }

    #[test]
    fn a_load_on_the_cpu_is_read_as_none_offloaded() {
        let r = parse(CPU);
        let l = r.layers.clone().unwrap();
        assert_eq!((l.total, l.offloaded, l.output_on_gpu), (25, 0, None));
        assert!(r.buffers.iter().all(|b| b.host), "every buffer is on the host");
        assert!(r.buffers.len() > 10, "the weights come in many mapped buffers");
        assert_eq!(r.fit.projected_mib, Some(398));
        // the planner lists the host, and the repacked copy of the weights the CPU kernels use
        assert_eq!(
            r.fit.breakdown.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["Host", "CPU_REPACK"]
        );
        let s = r.summary().unwrap();
        assert_eq!(s.gpu_model_mib, 0.0);
        assert_eq!(placement_from_layers(&s, false), Some("cpu"));
    }

    #[test]
    fn a_model_with_a_sliding_window_cache_has_two_caches_and_loads_in_stages() {
        let r = parse(GEMMA);
        assert_eq!(r.layers.as_ref().map(|l| (l.total, l.offloaded)), Some((61, 61)));
        assert_eq!(r.model.n_swa, Some(1024));
        assert!(close(r.model.file_size_mib, 10.95 * 1024.0));
        assert_eq!(r.model.params.as_deref(), Some("30.70 B"));
        // the cache of the full-attention layers and the one of the sliding-window layers
        let kv: Vec<&Cache> = r.caches.iter().filter(|c| c.kind == "kv").collect();
        assert_eq!(kv.len(), 2);
        assert_eq!(
            (kv[0].swa, kv[0].mib, kv[0].cells, kv[0].layers),
            (Some(false), 320.0, Some(4096), Some(10))
        );
        assert_eq!(
            (kv[1].swa, kv[1].mib, kv[1].cells, kv[1].layers),
            (Some(true), 1200.0, Some(1536), Some(50))
        );
        // a card with room: the fit changed nothing, and the planner's "unaccounted" is negative
        assert_eq!(r.fit.changed, Some(false));
        assert_eq!(r.fit.breakdown[0].unaccounted_mib, Some(-12102));
        let s = r.summary().unwrap();
        assert_eq!((s.gpu_model_mib, s.host_model_mib, s.kv_mib), (11214.40, 756.0, 1520.0));
        assert_eq!(placement_from_layers(&s, false), Some("gpu"));
        // the weights came off a hard drive: minutes between allocating the buffers and building the context
        assert!(r.stages.tensors_s.is_some_and(|s| s > 250.0), "{:?}", r.stages);
        assert!(r.stages.tensors_mib_per_s.is_some_and(|r| r < 100.0));
        assert!(r.stages.ready_s.is_some_and(|s| s > 300.0));
    }

    #[test]
    fn a_log_at_the_default_verbosity_says_what_it_lacks() {
        let r = parse(DEFAULT);
        assert_eq!(r.verbosity, Some(3));
        assert!(!r.detailed);
        assert!(r.layers.is_none() && r.summary().is_none() && r.buffers.is_empty());
        assert!(r.notes.iter().any(|n| n.contains("-lv 4")), "{:?}", r.notes);
        assert!(r.server.ready);
        // no layers, no verdict: the hub falls back to what the GPU holds
        assert_eq!(parse("").summary(), None);
        assert!(!parse("not a log\n\n  at all").detailed);
    }

    #[test]
    fn the_report_as_json_has_no_empty_fields() {
        let v = parse(GPU).to_json();
        assert_eq!(v["layers"]["offloaded"], 25);
        assert_eq!(v["caches"][0]["parts"][0]["name"], "K");
        fn bare(v: &Value, at: &str) {
            match v {
                Value::Null => panic!("null at {at}"),
                Value::Array(a) => {
                    assert!(!a.is_empty(), "empty list at {at}");
                    a.iter().enumerate().for_each(|(i, x)| bare(x, &format!("{at}[{i}]")));
                }
                Value::Object(o) => {
                    assert!(!o.is_empty(), "empty object at {at}");
                    o.iter().for_each(|(k, x)| bare(x, &format!("{at}.{k}")));
                }
                _ => {}
            }
        }
        for log in [GPU, PARTIAL, CPU, GEMMA, DEFAULT] {
            bare(&parse(log).to_json(), "report");
        }
        // a log without the details has no layers, buffers or devices to show, and none of them is printed as null or []
        let bare = parse(DEFAULT).to_json();
        for key in ["layers", "buffers", "devices", "caches", "fit"] {
            assert!(bare.get(key).is_none(), "{key} should be left out: {bare}");
        }
        assert!(bare["notes"].is_array());
        assert_eq!(v["fit"]["breakdown"][0]["unaccounted_mib"], 399);
    }

    #[test]
    fn what_the_fit_moves_off_the_card_is_noticed() {
        let log = "0.00.100.000 I cmn  common_fit_a: fitting params to device memory ...\n\
                   0.00.200.000 I common_params_fit_impl: projected to use 30000 MiB of device memory vs. 23743 MiB of free device memory\n\
                   0.00.200.001 I common_params_fit_impl: overriding tensor buffer types: experts of layers 20-39 go to the CPU\n\
                   0.00.200.002 I common_fit_params: successfully fit params to free device memory\n\
                   0.00.300.000 I load_tensors: offloaded 41/41 layers to GPU\n";
        let r = parse(log);
        assert_eq!(
            (r.fit.ok, r.fit.changed, r.fit.moved_tensors),
            (Some(true), Some(true), true)
        );
        let s = r.summary().unwrap();
        assert!(s.tensors_moved);
        // every layer is on the card, but the experts of half of them are not
        assert_eq!(placement_from_layers(&s, false), Some("partial"));
    }

    #[test]
    fn the_timings_after_each_answer_are_read_and_the_latest_are_kept() {
        let one = recent_requests(GPU, 10);
        assert_eq!(one.len(), 1);
        let t = &one[0];
        assert_eq!(
            (t.task, t.prompt_tokens, t.generated_tokens, t.total_tokens),
            (0, Some(20), Some(300), Some(320))
        );
        assert!(close(t.prompt_ms, 527.55) && close(t.generation_ms, 974.83) && close(t.total_ms, 1502.37));
        assert!(close(t.prompt_tokens_per_s, 37.91) && close(t.generation_tokens_per_s, 306.72));
        assert_eq!((t.graphs_reused, t.graph_reallocs), (Some(298), Some((0, 0))));
        assert_eq!((t.context_tokens, t.truncated), (Some(319), Some(false)));
        // three answers in a log: the last two are asked for
        let task = |n: u32| {
            GPU.lines()
                .filter(|l| l.contains("print_timing") || l.contains("release:"))
                .map(|l| l.replace("task 0", &format!("task {n}")))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = format!("{}\n{}\n{}\n", task(7), task(8), task(9));
        let last = recent_requests(&text, 2);
        assert_eq!(last.iter().map(|t| t.task).collect::<Vec<_>>(), [8, 9]);
        assert_eq!(last[1].context_tokens, Some(319));
        // an answer cut off before its total is not reported
        let cut = "0.01.000.000 I slot print_timing: id  0 | task 3 | prompt eval time = 5.00 ms / 2 tokens (2.50 ms per token, 400.00 tokens per second)";
        assert!(recent_requests(cut, 5).is_empty());
        let j = timing_json(t);
        assert_eq!(j["generated_tokens"], 300);
        assert_eq!(j["graph_reallocs"]["changed_size"], 0);
    }

    #[test]
    fn the_servers_counters_and_slots_are_read() {
        let page = "# HELP llamacpp:prompt_tokens_total Number of prompt tokens processed.\n\
                    # TYPE llamacpp:prompt_tokens_total counter\n\
                    llamacpp:prompt_tokens_total 31\n\
                    llamacpp:tokens_predicted_total 800\n\
                    llamacpp:tokens_predicted_seconds_total 3.80811\n\
                    llamacpp:predicted_tokens_seconds 209.815\n\
                    llamacpp:requests_processing 0\n";
        let m = parse_prometheus(page);
        assert_eq!(m.len(), 5);
        assert_eq!((m["prompt_tokens_total"], m["tokens_predicted_total"]), (31.0, 800.0));
        assert_eq!(m["predicted_tokens_seconds"], 209.815);
        let slots = json!([{"id": 0, "n_ctx": 4096, "is_processing": false, "id_task": 0, "n_prompt_tokens": 830,
                            "params": {"seed": 4294967295u64, "temperature": 0.7}}]);
        let s = slots_summary(&slots);
        assert_eq!(
            s,
            vec![json!({"id": 0, "n_ctx": 4096, "is_processing": false, "id_task": 0, "n_prompt_tokens": 830})]
        );
        assert!(slots_summary(&json!({"error": "no"})).is_empty());
    }

    #[test]
    fn the_hubs_own_lines_open_a_log() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_795_627_074), "2026-11-25T17:17:54Z");
        assert_eq!(iso_utc(4_102_444_799), "2099-12-31T23:59:59Z");
        let h = log_header(
            "0.4.2",
            0,
            "C:/engines/llama-server.exe",
            &["-m".into(), "m.gguf".into(), "-lv".into(), "4".into()],
            &[("GGML_VK_DISABLE_COOPMAT".into(), "1".into())],
            "gemma Asked",
        );
        assert_eq!(
            h,
            "# mm 0.4.2 1970-01-01T00:00:00Z: gemma Asked\n# mm server: C:/engines/llama-server.exe\n\
             # mm args: -m m.gguf -lv 4\n# mm env: GGML_VK_DISABLE_COOPMAT=1\n"
        );
        // the hub's lines are not mistaken for the server's
        assert_eq!(parse(&format!("{h}{GPU}")), parse(GPU));
    }

    #[test]
    fn an_owners_verbosity_and_expert_arguments_are_told_from_the_rest() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(sets_verbosity(&a(&["-lv", "3"])));
        assert!(sets_verbosity(&a(&["--log-verbosity=2"])) && sets_verbosity(&a(&["-v"])));
        assert!(!sets_verbosity(&a(&["-t", "8", "--metrics"])));
        assert!(moves_tensors(&a(&["--cpu-moe"])) && moves_tensors(&a(&["--n-cpu-moe", "12"])));
        assert!(moves_tensors(&a(&["-ot", ".ffn_.*_exps.=CPU"])));
        assert!(!moves_tensors(&a(&["--n-cpu-moe", "0"])) && !moves_tensors(&a(&["-ngl", "99"])));
    }

    #[test]
    fn nulls_and_empty_containers_are_pruned_at_every_depth() {
        let v =
            prune(json!({"a": null, "b": [], "c": {"d": null, "e": {}}, "f": 0, "g": false, "h": [{"i": null}, 1]}));
        assert_eq!(v, json!({"f": 0, "g": false, "h": [{}, 1]}));
    }
}

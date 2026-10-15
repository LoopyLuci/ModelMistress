//! Serving profiles and the VRAM planner.
//!
//! A *profile* says how one model is served by its `llama-server`: context, parallel slots, KV-cache types, flash
//! attention, prompt caching, batch sizes, chat template. They are typed (checked before a process is started) and
//! come from, strongest first: the load request, `[profiles."<model id>"]` in `config.toml`, `[serving]` in
//! `config.toml`, then the old top-level `ctx_size`, `gpu_layers` and `parallel` settings. A field nobody sets adds no
//! flag, so llama-server's own default applies and an existing config loads exactly as before.
//!
//! The planner estimates, from the GGUF header and the profile, how much VRAM a load takes, and decides against the
//! card's free memory (read from `llama-server --list-devices`) whether it fits, fits after unloading the least
//! recently used models, or must be refused instead of spilling into shared memory.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// KV-cache types llama-server accepts for `-ctk` / `-ctv`.
pub const KV_TYPES: [&str; 9] = ["f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1"];

/// The serving options that are not already `LoadOpts` fields (`ctx_size`, `gpu_layers`, `parallel`).
/// Every field is optional: unset means "no flag".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeFlags {
    /// Context tokens *per slot*. The server is started with `-c ctx_per_slot * parallel`, because llama-server's
    /// `-c` is the total that the slots share. Wins over `ctx_size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_per_slot: Option<u32>,
    /// KV-cache type for keys (`-ctk`): f16, q8_0, q4_0, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_type_k: Option<String>,
    /// KV-cache type for values (`-ctv`). A quantised V cache needs flash attention.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_type_v: Option<String>,
    /// Flash attention (`-fa`): "on", "off" or "auto".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_attn: Option<String>,
    /// Keep a slot's prompt between requests so the next turn only processes what is new (`--cache-prompt`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_prompt: Option<bool>,
    /// Smallest chunk (tokens) to reuse from the cache by shifting the KV (`--cache-reuse`); 0 = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_reuse: Option<u32>,
    /// RAM (MiB) for prompt caches kept outside the slots (`--cache-ram`); 0 = off, -1 = no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_ram_mib: Option<i64>,
    /// One KV buffer shared by all slots (`--kv-unified`): a slot may use as much as the others leave free.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kv_unified: Option<bool>,
    /// Logical batch size (`-b`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    /// Physical batch size (`-ub`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ubatch_size: Option<u32>,
    /// Use the Jinja chat template engine (`--jinja`), which tool calls need.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jinja: Option<bool>,
    /// A Jinja chat template file to use instead of the one in the GGUF (`--chat-template-file`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_file: Option<PathBuf>,
}

impl ServeFlags {
    /// `self` over `base`: each field of `self` that is set wins.
    pub fn over(&self, base: &ServeFlags) -> ServeFlags {
        ServeFlags {
            ctx_per_slot: self.ctx_per_slot.or(base.ctx_per_slot),
            cache_type_k: self.cache_type_k.clone().or_else(|| base.cache_type_k.clone()),
            cache_type_v: self.cache_type_v.clone().or_else(|| base.cache_type_v.clone()),
            flash_attn: self.flash_attn.clone().or_else(|| base.flash_attn.clone()),
            cache_prompt: self.cache_prompt.or(base.cache_prompt),
            cache_reuse: self.cache_reuse.or(base.cache_reuse),
            cache_ram_mib: self.cache_ram_mib.or(base.cache_ram_mib),
            kv_unified: self.kv_unified.or(base.kv_unified),
            batch_size: self.batch_size.or(base.batch_size),
            ubatch_size: self.ubatch_size.or(base.ubatch_size),
            jinja: self.jinja.or(base.jinja),
            chat_template_file: self
                .chat_template_file
                .clone()
                .or_else(|| base.chat_template_file.clone()),
        }
    }
}

/// One entry of `[profiles]` (a model's) or the `[serving]` table (every model's).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeProfile {
    /// Total context tokens shared by the slots (0 = the model's own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_layers: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<u32>,
    #[serde(flatten)]
    pub flags: ServeFlags,
    /// Anything else in the table: a misspelt key is reported by [`ServeProfile::check`], not ignored.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

impl ServeProfile {
    /// Errors for keys that are not serving options, and for bad values.
    pub fn check(&self, what: &str) -> Result<(), String> {
        if let Some(k) = self.unknown.keys().next() {
            return Err(format!("{what}: unknown serving option \"{k}\""));
        }
        validate_flags(&self.flags, self.parallel).map_err(|e| format!("{what}: {e}"))
    }
}

impl ServeProfile {
    /// Reads a `[serving]` or `[profiles.x]` table without ever failing: a key that is not a serving option, a value of
    /// the wrong type and a value `validate_flags` refuses each become a warning and that one field keeps its default
    /// (unset), so a typo never stops the hub. `check` stays strict for config.set.
    pub fn lenient(table: &toml::Table, what: &str) -> (ServeProfile, Vec<String>) {
        let mut warnings = vec![];
        let mut good = toml::Table::new();
        for (k, v) in table {
            let mut one = toml::Table::new();
            one.insert(k.clone(), v.clone());
            match toml::Value::Table(one).try_into::<ServeProfile>() {
                Err(e) => warnings.push(format!(
                    "{what}: \"{k}\" ignored (default used): {}",
                    e.message().trim()
                )),
                Ok(p) if !p.unknown.is_empty() => {
                    warnings.push(format!("{what}: unknown serving option \"{k}\" ignored"))
                }
                Ok(_) => {
                    good.insert(k.clone(), v.clone());
                }
            }
        }
        let mut p: ServeProfile = toml::Value::Table(good).try_into().unwrap_or_default();
        p.sanitize(what, &mut warnings);
        (p, warnings)
    }

    /// Unsets each field whose value `validate_flags` would refuse, with a warning.
    fn sanitize(&mut self, what: &str, warnings: &mut Vec<String>) {
        let mut drop = |name: &str, why: String| warnings.push(format!("{what}: {name} ignored (default used): {why}"));
        for (name, t) in [
            ("cache_type_k", &mut self.flags.cache_type_k),
            ("cache_type_v", &mut self.flags.cache_type_v),
        ] {
            if t.as_deref().is_some_and(|v| !kv_type_ok(v)) {
                drop(
                    name,
                    format!(
                        "\"{}\" is not one of {}",
                        t.take().unwrap_or_default(),
                        KV_TYPES.join(", ")
                    ),
                );
            }
        }
        let f = &mut self.flags;
        if f.flash_attn
            .as_deref()
            .is_some_and(|v| !matches!(v, "on" | "off" | "auto"))
        {
            let v = f.flash_attn.take().unwrap_or_default();
            drop("flash_attn", format!("\"{v}\" must be \"on\", \"off\" or \"auto\""));
        }
        if let Some(v) = f.cache_type_v.clone() {
            if !matches!(v.as_str(), "f32" | "f16" | "bf16") && f.flash_attn.as_deref() == Some("off") {
                f.cache_type_v = None;
                drop(
                    "cache_type_v",
                    format!("\"{v}\" needs flash attention, but flash_attn is \"off\""),
                );
            }
        }
        if self.parallel == Some(0) {
            self.parallel = None;
            drop("parallel", "must be at least 1".into());
        }
        if f.ctx_per_slot == Some(0) {
            f.ctx_per_slot = None;
            drop("ctx_per_slot", "must be at least 1".into());
        }
        for (name, n) in [("batch_size", &mut f.batch_size), ("ubatch_size", &mut f.ubatch_size)] {
            if *n == Some(0) {
                *n = None;
                drop(name, "must be at least 1".into());
            }
        }
        if let (Some(b), Some(ub)) = (f.batch_size, f.ubatch_size) {
            if ub > b {
                f.ubatch_size = None;
                drop("ubatch_size", format!("{ub} is larger than batch_size {b}"));
            }
        }
        if f.cache_ram_mib.is_some_and(|n| n < -1) {
            f.cache_ram_mib = None;
            drop(
                "cache_ram_mib",
                "must be -1 (no limit), 0 (off) or a size in MiB".into(),
            );
        }
        if f.cache_reuse.is_some() && f.cache_prompt == Some(false) {
            f.cache_reuse = None;
            drop("cache_reuse", "needs cache_prompt".into());
        }
        if f.chat_template_file.as_ref().is_some_and(|p| !p.is_file()) {
            let p = f.chat_template_file.take().unwrap_or_default();
            drop("chat_template_file", format!("{} is not a file", p.display()));
        }
    }
}

fn kv_type_ok(t: &str) -> bool {
    KV_TYPES.contains(&t)
}

/// Checks the typed values (not what the model or the build supports: llama-server says that when it starts).
pub fn validate_flags(f: &ServeFlags, parallel: Option<u32>) -> Result<(), String> {
    for (name, t) in [("cache_type_k", &f.cache_type_k), ("cache_type_v", &f.cache_type_v)] {
        if let Some(t) = t {
            if !kv_type_ok(t) {
                return Err(format!("{name} \"{t}\" is not one of {}", KV_TYPES.join(", ")));
            }
        }
    }
    if let Some(v) = &f.cache_type_v {
        let quantised = !matches!(v.as_str(), "f32" | "f16" | "bf16");
        if quantised && f.flash_attn.as_deref() == Some("off") {
            return Err(format!(
                "cache_type_v \"{v}\" needs flash attention, but flash_attn is \"off\""
            ));
        }
    }
    if let Some(fa) = &f.flash_attn {
        if !matches!(fa.as_str(), "on" | "off" | "auto") {
            return Err(format!("flash_attn \"{fa}\" must be \"on\", \"off\" or \"auto\""));
        }
    }
    if parallel == Some(0) {
        return Err("parallel must be at least 1".into());
    }
    if f.ctx_per_slot == Some(0) {
        return Err("ctx_per_slot must be at least 1".into());
    }
    if f.batch_size == Some(0) || f.ubatch_size == Some(0) {
        return Err("batch_size and ubatch_size must be at least 1".into());
    }
    if let (Some(b), Some(ub)) = (f.batch_size, f.ubatch_size) {
        if ub > b {
            return Err(format!("ubatch_size {ub} is larger than batch_size {b}"));
        }
    }
    if f.cache_ram_mib.is_some_and(|n| n < -1) {
        return Err("cache_ram_mib must be -1 (no limit), 0 (off) or a size in MiB".into());
    }
    if f.cache_reuse.is_some() && f.cache_prompt == Some(false) {
        return Err("cache_reuse needs cache_prompt".into());
    }
    if let Some(p) = &f.chat_template_file {
        if !p.is_file() {
            return Err(format!("chat_template_file {} is not a file", p.display()));
        }
    }
    Ok(())
}

/// What a model is started with after the layers of options are merged.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// `-c`: tokens of context in all (0 = the model's own).
    pub ctx_total: u32,
    pub slots: u32,
    pub gpu_layers: i32,
    pub flags: ServeFlags,
}

impl Resolved {
    /// Merges load request > the model's profile > `[serving]` > the old top-level settings.
    pub fn new(
        load: (Option<u32>, Option<i32>, Option<u32>, &ServeFlags),
        model: Option<&ServeProfile>,
        all: &ServeProfile,
        legacy: (u32, i32, u32),
    ) -> Resolved {
        let (ctx, ngl, par, flags) = load;
        let pick = |a: Option<u32>, f: fn(&ServeProfile) -> Option<u32>, d: u32| {
            a.or_else(|| model.and_then(f)).or_else(|| f(all)).unwrap_or(d)
        };
        let slots = pick(par, |p| p.parallel, legacy.2).max(1);
        let gpu_layers = ngl
            .or_else(|| model.and_then(|p| p.gpu_layers))
            .or(all.gpu_layers)
            .unwrap_or(legacy.1);
        let mut merged = flags.clone();
        if let Some(m) = model {
            merged = merged.over(&m.flags);
        }
        merged = merged.over(&all.flags);
        // The strongest layer that sets a context decides; within a layer a per-slot size wins over a total.
        let layers = [
            (ctx, flags.ctx_per_slot),
            (model.and_then(|p| p.ctx_size), model.and_then(|p| p.flags.ctx_per_slot)),
            (all.ctx_size, all.flags.ctx_per_slot),
        ];
        let mut ctx_total = legacy.0;
        merged.ctx_per_slot = None;
        for (total, per) in layers {
            if let Some(per) = per {
                ctx_total = per.saturating_mul(slots);
                merged.ctx_per_slot = Some(per);
                break;
            }
            if let Some(total) = total {
                ctx_total = total;
                break;
            }
        }
        Resolved {
            ctx_total,
            slots,
            gpu_layers,
            flags: merged,
        }
    }

    /// Context tokens one slot gets when the slots do not share (llama-server divides `-c` by the slots then).
    pub fn ctx_per_slot(&self) -> u32 {
        if self.ctx_total == 0 {
            0
        } else {
            self.ctx_total / self.slots.max(1)
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_flags(&self.flags, Some(self.slots))
    }

    /// The llama-server arguments. `-c`, `-ngl` and `-np` always; the rest only for what is set.
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-c".into(),
            self.ctx_total.to_string(),
            "-ngl".into(),
            self.gpu_layers.to_string(),
            "-np".into(),
            self.slots.to_string(),
        ];
        let f = &self.flags;
        let mut kv = |flag: &str, v: &Option<String>| {
            if let Some(v) = v {
                a.extend([flag.to_string(), v.clone()]);
            }
        };
        kv("-ctk", &f.cache_type_k);
        kv("-ctv", &f.cache_type_v);
        kv("-fa", &f.flash_attn);
        if let Some(b) = f.cache_prompt {
            a.push(if b { "--cache-prompt" } else { "--no-cache-prompt" }.into());
        }
        if let Some(n) = f.cache_reuse {
            a.extend(["--cache-reuse".into(), n.to_string()]);
        }
        if let Some(n) = f.cache_ram_mib {
            a.extend(["--cache-ram".into(), n.to_string()]);
        }
        if let Some(b) = f.kv_unified {
            a.push(if b { "--kv-unified" } else { "--no-kv-unified" }.into());
        }
        if let Some(n) = f.batch_size {
            a.extend(["-b".into(), n.to_string()]);
        }
        if let Some(n) = f.ubatch_size {
            a.extend(["-ub".into(), n.to_string()]);
        }
        if let Some(b) = f.jinja {
            a.push(if b { "--jinja" } else { "--no-jinja" }.into());
        }
        if let Some(p) = &f.chat_template_file {
            a.extend(["--chat-template-file".into(), p.display().to_string()]);
        }
        a
    }
}

/// What a loaded model reports about how it is served (`model.loaded`, `/v1/models`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Serving {
    /// Context tokens in all, as the server says (`/props`), else as asked.
    pub n_ctx: u64,
    /// What one slot can hold: `n_ctx / slots`, or all of `n_ctx` when the slots share one KV buffer.
    pub n_ctx_per_slot: u64,
    pub slots: u64,
    pub kv_unified: bool,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub flash_attn: String,
    pub cache_prompt: bool,
    pub batch_size: Option<u32>,
    pub ubatch_size: Option<u32>,
    /// Device memory the server put on GPUs (model + KV + compute buffers), from its log.
    pub vram_mib: Option<f64>,
    /// What the planner expected before the load, for comparison.
    pub estimated_vram_mib: Option<u64>,
    /// Where `n_ctx` came from: "server" (asked /props) or "requested".
    pub source: &'static str,
}

impl Serving {
    pub fn from_resolved(r: &Resolved) -> Serving {
        let unified = r.flags.kv_unified.unwrap_or(false);
        Serving {
            n_ctx: r.ctx_total as u64,
            n_ctx_per_slot: if unified {
                r.ctx_total as u64
            } else {
                r.ctx_per_slot() as u64
            },
            slots: r.slots as u64,
            kv_unified: unified,
            cache_type_k: r.flags.cache_type_k.clone().unwrap_or_else(|| "f16".into()),
            cache_type_v: r.flags.cache_type_v.clone().unwrap_or_else(|| "f16".into()),
            flash_attn: r.flags.flash_attn.clone().unwrap_or_else(|| "auto".into()),
            cache_prompt: r.flags.cache_prompt.unwrap_or(true),
            batch_size: r.flags.batch_size,
            ubatch_size: r.flags.ubatch_size,
            vram_mib: None,
            estimated_vram_mib: None,
            source: "requested",
        }
    }

    /// Takes the numbers a running server states in its `/props` answer.
    pub fn apply_props(&mut self, props: &Value) {
        let slots = props["total_slots"].as_u64().filter(|n| *n > 0);
        let n_ctx = props["default_generation_settings"]["n_ctx"]
            .as_u64()
            .filter(|n| *n > 0);
        if let Some(s) = slots {
            self.slots = s;
        }
        if let Some(c) = n_ctx {
            // /props reports the context one slot has; with a shared KV buffer that is the whole buffer.
            self.n_ctx_per_slot = c;
            self.n_ctx = if self.kv_unified { c } else { c * self.slots.max(1) };
            self.source = "server";
        }
    }
}

/// MiB the server's log says it put on GPU devices. Lines look like `load_tensors:      Vulkan0 model buffer size
/// = 17000.00 MiB`; host and CPU buffers are not VRAM.
pub fn vram_from_log(text: &str) -> Option<f64> {
    let mut seen: Vec<(String, f64)> = vec![];
    for line in text.lines() {
        let Some((head, tail)) = line.split_once(" buffer size =") else {
            continue;
        };
        let words: Vec<&str> = head.split_whitespace().collect();
        if words.len() < 2 {
            continue;
        }
        let device = words[words.len() - 2];
        let upper = device.to_ascii_uppercase();
        if upper.starts_with("CPU") || upper.contains("HOST") || upper.contains("MAPPED") {
            continue;
        }
        let Some(mib) = tail.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()) else {
            continue;
        };
        let key = format!("{device} {}", words[words.len() - 1]);
        if !seen.iter().any(|(k, _)| *k == key) {
            seen.push((key, mib));
        }
    }
    (!seen.is_empty()).then(|| seen.iter().map(|(_, m)| m).sum())
}

// ---- estimating ----------------------------------------------------------------------------------------------

/// What the planner needs to know about a model, read from its GGUF header.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelShape {
    pub file_bytes: u64,
    /// Transformer layers that are served (the multi-token-prediction layer a file may carry is left out).
    pub layers: u64,
    /// Of those, the ones with a KV cache (a hybrid model has attention in every Nth layer only).
    pub attn_layers: u64,
    pub n_embd: u64,
    pub n_head_kv: u64,
    pub key_len: u64,
    pub val_len: u64,
    /// Bytes of recurrent state one slot needs (0 for a pure transformer).
    pub state_bytes_per_slot: u64,
    /// Fraction of the weights that are expert tensors (0 for a dense model).
    pub expert_share: f64,
}

fn meta_u64(meta: &Map<String, Value>, key: &str) -> Option<u64> {
    meta.get(key).and_then(Value::as_u64)
}

/// The shape from the scalar GGUF metadata (`<arch>.*`). None when the layers or widths are not in it.
pub fn shape_from_meta(meta: &Map<String, Value>, file_bytes: u64) -> Option<ModelShape> {
    let arch = meta.get("general.architecture")?.as_str()?;
    let k = |s: &str| meta_u64(meta, &format!("{arch}.{s}"));
    let blocks = k("block_count")?;
    let nextn = k("nextn_predict_layers").unwrap_or(0);
    let layers = blocks.saturating_sub(nextn).max(1);
    let n_embd = k("embedding_length")?;
    let n_head = k("attention.head_count")?.max(1);
    let n_head_kv = k("attention.head_count_kv").unwrap_or(n_head);
    let key_len = k("attention.key_length").unwrap_or(n_embd / n_head);
    let val_len = k("attention.value_length").unwrap_or(key_len);
    let interval = k("full_attention_interval").filter(|n| *n > 1);
    let attn_layers = match interval {
        Some(n) => layers / n,
        None => layers,
    };
    let state_bytes_per_slot = match (interval, k("ssm.state_size"), k("ssm.inner_size")) {
        // gated delta-net layers: a state_size x inner_size recurrent matrix plus the convolution window, in f32
        (Some(_), Some(state), Some(inner)) => {
            let groups = k("ssm.group_count").unwrap_or(0);
            let conv = k("ssm.conv_kernel").unwrap_or(4).saturating_sub(1) * (inner + 2 * groups * state) * 4;
            (layers - attn_layers) * (state * inner * 4 + conv)
        }
        _ => 0,
    };
    let expert_share = match (k("expert_count"), k("expert_feed_forward_length")) {
        (Some(n), Some(ff)) if n > 0 => {
            let experts = (layers * 3 * n_embd * ff * n) as f64;
            let attn = (attn_layers * n_embd * (2 * n_head * key_len + 2 * n_head_kv * key_len)) as f64;
            let embeddings = (2 * 152_000 * n_embd) as f64;
            experts / (experts + attn + embeddings)
        }
        _ => 0.0,
    };
    Some(ModelShape {
        file_bytes,
        layers,
        attn_layers,
        n_embd,
        n_head_kv,
        key_len,
        val_len,
        state_bytes_per_slot,
        expert_share,
    })
}

/// Bytes one element of a KV type takes.
pub fn kv_bytes_per_elem(t: &str) -> f64 {
    match t {
        "f32" => 4.0,
        "f16" | "bf16" => 2.0,
        "q8_0" => 34.0 / 32.0,
        "q5_0" => 22.0 / 32.0,
        "q5_1" => 24.0 / 32.0,
        "q4_1" => 20.0 / 32.0,
        "q4_0" | "iq4_nl" => 18.0 / 32.0,
        _ => 2.0,
    }
}

/// How the model is placed, for the estimate.
#[derive(Debug, Clone)]
pub struct Placement {
    pub ctx_total: u64,
    pub slots: u64,
    pub gpu_layers: i32,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub ubatch: u64,
    /// `--cpu-moe`
    pub cpu_moe: bool,
    /// `--n-cpu-moe N`
    pub n_cpu_moe: u64,
    /// Extra bytes already known to sit on the GPU (a draft model's file).
    pub extra_gpu_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VramEstimate {
    pub weights_mib: u64,
    pub kv_mib: u64,
    pub state_mib: u64,
    pub compute_mib: u64,
    pub total_mib: u64,
}

const MIB: f64 = 1_048_576.0;
/// Fixed cost of a context on the GPU (driver, command buffers) and the compute graph per 512 tokens of ubatch.
const CONTEXT_OVERHEAD_MIB: f64 = 200.0;
const COMPUTE_MIB_PER_512_UBATCH: f64 = 300.0;

/// VRAM a load takes, from the shape, the context and the KV types. An estimate, within a few hundred MiB for
/// the models measured (see docs/serving.md); the planner keeps a margin for that.
pub fn estimate_vram(shape: &ModelShape, p: &Placement) -> VramEstimate {
    if p.gpu_layers == 0 {
        return VramEstimate {
            weights_mib: 0,
            kv_mib: 0,
            state_mib: 0,
            compute_mib: 0,
            total_mib: 0,
        };
    }
    let on_gpu = if p.gpu_layers < 0 {
        1.0
    } else {
        (p.gpu_layers as f64 / shape.layers.max(1) as f64).min(1.0)
    };
    let mut weights = shape.file_bytes as f64;
    if shape.expert_share > 0.0 {
        let on_cpu = if p.cpu_moe {
            shape.expert_share
        } else if p.n_cpu_moe > 0 {
            shape.expert_share * (p.n_cpu_moe as f64 / shape.layers.max(1) as f64).min(1.0)
        } else {
            0.0
        };
        weights -= weights * on_cpu;
    }
    weights = weights * on_gpu + p.extra_gpu_bytes as f64;
    let per_token = shape.attn_layers as f64
        * shape.n_head_kv as f64
        * (shape.key_len as f64 * kv_bytes_per_elem(&p.cache_type_k)
            + shape.val_len as f64 * kv_bytes_per_elem(&p.cache_type_v));
    let kv = per_token * p.ctx_total as f64 * on_gpu;
    let state = shape.state_bytes_per_slot as f64 * p.slots as f64 * on_gpu;
    let compute = COMPUTE_MIB_PER_512_UBATCH * p.ubatch as f64 / 512.0 + CONTEXT_OVERHEAD_MIB;
    let r = |b: f64| (b / MIB).ceil() as u64;
    let (w, k, s) = (r(weights), r(kv), r(state));
    let c = compute.ceil() as u64;
    VramEstimate {
        weights_mib: w,
        kv_mib: k,
        state_mib: s,
        compute_mib: c,
        total_mib: w + k + s + c,
    }
}

// ---- planning ------------------------------------------------------------------------------------------------

/// A model that is loaded now, as the planner sees it.
#[derive(Debug, Clone)]
pub struct Resident {
    pub model: String,
    pub idle_s: u64,
    pub vram_mib: u64,
    /// Requests running on it: it cannot be unloaded to free memory before they finish.
    pub busy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Free memory is enough as it is.
    Fits,
    /// Enough after these models (least recently used first) are unloaded.
    Evict(Vec<String>),
    /// Not enough even with every idle model unloaded.
    Refuse { reclaimable_mib: u64 },
}

/// Decides whether `need_mib` (plus `margin_mib`) fits in `free_mib`, unloading idle models least recently used
/// first when it does not.
pub fn plan(need_mib: u64, free_mib: u64, margin_mib: u64, resident: &[Resident]) -> Plan {
    let want = need_mib + margin_mib;
    if free_mib >= want {
        return Plan::Fits;
    }
    let mut idle: Vec<&Resident> = resident.iter().filter(|r| !r.busy).collect();
    idle.sort_by_key(|r| std::cmp::Reverse(r.idle_s));
    let mut have = free_mib;
    let mut evict = vec![];
    for r in idle.iter() {
        if have >= want {
            break;
        }
        have += r.vram_mib;
        evict.push(r.model.clone());
    }
    if have >= want {
        Plan::Evict(evict)
    } else {
        Plan::Refuse {
            reclaimable_mib: idle.iter().map(|r| r.vram_mib).sum(),
        }
    }
}

/// One GPU from `llama-server --list-devices`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Device {
    pub name: String,
    pub total_mib: u64,
    pub free_mib: u64,
}

/// Parses `  Vulkan0: AMD Radeon RX 7900 XTX (24560 MiB, 23743 MiB free)` lines. CPU entries are skipped.
pub fn parse_devices(text: &str) -> Vec<Device> {
    let mut out = vec![];
    for line in text.lines() {
        let line = line.trim();
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.to_ascii_uppercase().starts_with("CPU") || name.contains(' ') {
            continue;
        }
        let Some(open) = rest.rfind('(') else { continue };
        let inner = rest[open + 1..].trim_end_matches(')');
        let nums: Vec<u64> = inner
            .split(',')
            .filter_map(|p| p.split_whitespace().next().and_then(|n| n.parse().ok()))
            .collect();
        if let [total, free] = nums[..] {
            out.push(Device {
                name: format!("{name}: {}", rest[..open].trim()),
                total_mib: total,
                free_mib: free,
            });
        }
    }
    out
}

/// Reads `typeperf ... -sc 1` output (CSV: a header of counter paths, one row of values) into (counter, value).
pub fn parse_typeperf(text: &str) -> Vec<(String, f64)> {
    let rows: Vec<Vec<String>> = text
        .lines()
        .filter(|l| l.starts_with('"'))
        .map(|l| l.trim().trim_matches('"').split("\",\"").map(str::to_string).collect())
        .collect();
    let [head, vals, ..] = &rows[..] else {
        return vec![];
    };
    head.iter()
        .zip(vals)
        .skip(1)
        .filter_map(|(h, v)| v.trim().parse::<f64>().ok().map(|v| (h.clone(), v)))
        .collect()
}

/// Dedicated GPU memory in use on the busiest adapter, in MiB, from Windows' performance counters.
pub fn adapter_used_mib(typeperf: &str) -> Option<u64> {
    parse_typeperf(typeperf)
        .into_iter()
        .map(|(_, b)| b)
        .fold(None, |m: Option<f64>, b| Some(m.map_or(b, |m| m.max(b))))
        .map(|b| (b / MIB).round() as u64)
}

/// Dedicated GPU memory one process holds, in MiB.
pub fn process_used_mib(typeperf: &str) -> Option<u64> {
    let v = parse_typeperf(typeperf);
    (!v.is_empty()).then(|| (v.iter().map(|(_, b)| b).sum::<f64>() / MIB).round() as u64)
}

/// A one-line JSON view of the estimate for the API.
pub fn estimate_json(e: &VramEstimate) -> Value {
    json!({"weights_mib": e.weights_mib, "kv_mib": e.kv_mib, "state_mib": e.state_mib,
           "compute_mib": e.compute_mib, "total_mib": e.total_mib})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// Shaped like a 48-layer, 4-KV-head, 128-wide-head mixture-of-experts model.
    fn moe_meta() -> Map<String, Value> {
        meta(&[
            ("general.architecture", json!("tmoe")),
            ("tmoe.block_count", json!(48)),
            ("tmoe.embedding_length", json!(2048)),
            ("tmoe.attention.head_count", json!(32)),
            ("tmoe.attention.head_count_kv", json!(4)),
            ("tmoe.attention.key_length", json!(128)),
            ("tmoe.attention.value_length", json!(128)),
            ("tmoe.expert_count", json!(128)),
            ("tmoe.expert_feed_forward_length", json!(768)),
        ])
    }

    /// Shaped like a hybrid: 32 layers, attention in every 4th, plus one extra prediction layer.
    fn hybrid_meta() -> Map<String, Value> {
        meta(&[
            ("general.architecture", json!("thyb")),
            ("thyb.block_count", json!(33)),
            ("thyb.nextn_predict_layers", json!(1)),
            ("thyb.embedding_length", json!(4096)),
            ("thyb.attention.head_count", json!(16)),
            ("thyb.attention.head_count_kv", json!(4)),
            ("thyb.attention.key_length", json!(256)),
            ("thyb.attention.value_length", json!(256)),
            ("thyb.full_attention_interval", json!(4)),
            ("thyb.ssm.conv_kernel", json!(4)),
            ("thyb.ssm.state_size", json!(128)),
            ("thyb.ssm.group_count", json!(16)),
            ("thyb.ssm.inner_size", json!(4096)),
        ])
    }

    fn place(ctx: u64, slots: u64, k: &str, v: &str) -> Placement {
        Placement {
            ctx_total: ctx,
            slots,
            gpu_layers: 999,
            cache_type_k: k.into(),
            cache_type_v: v.into(),
            ubatch: 512,
            cpu_moe: false,
            n_cpu_moe: 0,
            extra_gpu_bytes: 0,
        }
    }

    #[test]
    fn kv_size_follows_the_formula_and_the_type() {
        let shape = shape_from_meta(&moe_meta(), 17 << 30).unwrap();
        assert_eq!((shape.layers, shape.attn_layers), (48, 48));
        // 48 layers x 4 kv heads x (128 + 128) elements x 2 bytes = 98304 bytes a token; x 32768 tokens = 3 GiB
        let f16 = estimate_vram(&shape, &place(32768, 1, "f16", "f16"));
        assert_eq!(f16.kv_mib, 3072);
        let q8 = estimate_vram(&shape, &place(32768, 1, "q8_0", "q8_0"));
        assert_eq!(q8.kv_mib, 1632); // 34/32 bytes an element instead of 2
        let q4 = estimate_vram(&shape, &place(32768, 1, "q4_0", "q4_0"));
        assert!(q4.kv_mib < q8.kv_mib);
        // the context is shared by the slots: more slots at the same -c cost no more KV
        assert_eq!(estimate_vram(&shape, &place(32768, 4, "f16", "f16")).kv_mib, f16.kv_mib);
        // and twice the context costs twice the KV
        assert_eq!(
            estimate_vram(&shape, &place(65536, 1, "f16", "f16")).kv_mib,
            f16.kv_mib * 2
        );
    }

    #[test]
    fn a_hybrid_has_kv_in_every_fourth_layer_and_state_per_slot() {
        let shape = shape_from_meta(&hybrid_meta(), 5 << 30).unwrap();
        assert_eq!((shape.layers, shape.attn_layers), (32, 8));
        // 24 recurrent layers x (128 x 4096 x 4 B state + 3 x (4096 + 2 x 16 x 128) x 4 B conv window)
        assert_eq!(shape.state_bytes_per_slot, 24 * (2_097_152 + 98_304));
        let one = estimate_vram(&shape, &place(4096, 1, "f16", "f16"));
        let four = estimate_vram(&shape, &place(4096, 4, "f16", "f16"));
        assert_eq!(four.kv_mib, one.kv_mib);
        assert!(four.state_mib >= one.state_mib * 4 - 3);
        // 8 layers x 4 kv heads x 512 elements x 2 bytes x 4096 tokens = 128 MiB
        assert_eq!(one.kv_mib, 128);
    }

    #[test]
    fn experts_on_the_cpu_leave_the_gpu_with_the_rest() {
        let shape = shape_from_meta(&moe_meta(), 20 << 30).unwrap();
        assert!(shape.expert_share > 0.9 && shape.expert_share < 1.0);
        let all = estimate_vram(&shape, &place(8192, 1, "f16", "f16"));
        let cpu = estimate_vram(
            &shape,
            &Placement {
                cpu_moe: true,
                ..place(8192, 1, "f16", "f16")
            },
        );
        assert!(cpu.weights_mib < all.weights_mib / 8);
        let half = estimate_vram(
            &shape,
            &Placement {
                n_cpu_moe: 24,
                ..place(8192, 1, "f16", "f16")
            },
        );
        assert!(half.weights_mib > cpu.weights_mib && half.weights_mib < all.weights_mib);
    }

    #[test]
    fn partial_offload_and_cpu_only() {
        let shape = shape_from_meta(&hybrid_meta(), 8 << 30).unwrap();
        let full = estimate_vram(&shape, &place(8192, 2, "f16", "f16"));
        let half = estimate_vram(
            &shape,
            &Placement {
                gpu_layers: 16,
                ..place(8192, 2, "f16", "f16")
            },
        );
        assert!(half.weights_mib <= full.weights_mib / 2 + 1);
        let cpu = estimate_vram(
            &shape,
            &Placement {
                gpu_layers: 0,
                ..place(8192, 2, "f16", "f16")
            },
        );
        assert_eq!(cpu.total_mib, 0);
    }

    #[test]
    fn metadata_without_layers_gives_no_shape() {
        let m = meta(&[("general.architecture", json!("x")), ("x.embedding_length", json!(64))]);
        assert!(shape_from_meta(&m, 1).is_none());
        assert!(shape_from_meta(&Map::new(), 1).is_none());
    }

    fn res(m: &str, idle: u64, mib: u64, busy: bool) -> Resident {
        Resident {
            model: m.into(),
            idle_s: idle,
            vram_mib: mib,
            busy,
        }
    }

    #[test]
    fn the_planner_fits_evicts_least_recently_used_or_refuses() {
        let loaded = [
            res("a", 10, 6000, false),
            res("b", 900, 8000, false),
            res("c", 500, 4000, true),
        ];
        assert_eq!(plan(5000, 9000, 512, &loaded), Plan::Fits);
        // 3000 free, need 10000+512: the longest idle (b) first, then a
        assert_eq!(plan(10_000, 3000, 512, &loaded), Plan::Evict(vec!["b".into()]));
        assert_eq!(
            plan(16_000, 3000, 512, &loaded),
            Plan::Evict(vec!["b".into(), "a".into()])
        );
        // c is busy and its 4000 do not count
        assert_eq!(
            plan(20_000, 3000, 512, &loaded),
            Plan::Refuse {
                reclaimable_mib: 14_000
            }
        );
        assert_eq!(plan(100, 0, 0, &[]), Plan::Refuse { reclaimable_mib: 0 });
        // the margin counts
        assert_eq!(plan(1000, 1200, 512, &[]), Plan::Refuse { reclaimable_mib: 0 });
        assert_eq!(plan(1000, 1512, 512, &[]), Plan::Fits);
    }

    #[test]
    fn devices_are_read_from_the_listing() {
        let text = "ggml_vulkan: Found 1 Vulkan devices:\nAvailable devices:\n  Vulkan0: AMD Radeon RX 7900 XTX (24560 MiB, 23743 MiB free)\n  CPU: x (1 MiB, 1 MiB free)\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].total_mib, d[0].free_mib), (24560, 23743));
        assert!(d[0].name.starts_with("Vulkan0"));
        assert!(parse_devices("Available devices:\n  (none)\n").is_empty());
    }

    #[test]
    fn windows_counters_give_the_memory_in_use() {
        let adapters = concat!(
            "
\"(PDH-CSV 4.0)\",\"HOST adapter a Dedicated Usage\",\"HOST adapter b Dedicated Usage\"
",
            "\"10/13/2026 15:53:45.029\",\"23000440832.000000\",\"0.000000\"
Exiting, please wait...
"
        );
        assert_eq!(adapter_used_mib(adapters), Some(21935));
        let process = "\"(PDH-CSV 4.0)\",\"HOST pid_7 Dedicated Usage\"
\"t\",\"19429396480.000000\"
";
        assert_eq!(process_used_mib(process), Some(18529));
        assert_eq!(
            adapter_used_mib(
                "Error: no counters
"
            ),
            None
        );
        assert_eq!(process_used_mib(""), None);
    }

    #[test]
    fn vram_comes_from_device_buffers_only() {
        let log = "load_tensors:      Vulkan0 model buffer size = 17000.50 MiB\n\
                   load_tensors:   CPU_Mapped model buffer size =   500.00 MiB\n\
                   llama_kv_cache:      Vulkan0 KV buffer size =  1632.00 MiB\n\
                   sched_reserve:      Vulkan0 compute buffer size =   300.00 MiB\n\
                   sched_reserve: Vulkan_Host compute buffer size =    30.00 MiB\n\
                   llama_kv_cache:      Vulkan0 KV buffer size =  1632.00 MiB\n";
        assert_eq!(vram_from_log(log), Some(17000.5 + 1632.0 + 300.0));
        assert_eq!(vram_from_log("nothing here"), None);
    }

    #[test]
    fn layers_of_options_merge_in_order() {
        let none = ServeFlags::default();
        let mut all = ServeProfile::default();
        all.flags.flash_attn = Some("on".into());
        all.flags.cache_type_k = Some("f16".into());
        let mut model = ServeProfile {
            parallel: Some(4),
            ..Default::default()
        };
        model.flags.ctx_per_slot = Some(32768);
        model.flags.cache_type_k = Some("q8_0".into());
        model.flags.cache_type_v = Some("q8_0".into());
        let r = Resolved::new((None, None, None, &none), Some(&model), &all, (8192, 999, 2));
        assert_eq!((r.ctx_total, r.slots, r.gpu_layers), (131_072, 4, 999));
        assert_eq!(r.flags.cache_type_k.as_deref(), Some("q8_0")); // the model's beats [serving]
        assert_eq!(r.flags.flash_attn.as_deref(), Some("on")); // [serving] fills what the model leaves
        let a = r.args();
        assert_eq!(&a[..6], ["-c", "131072", "-ngl", "999", "-np", "4"]);
        for pair in [["-ctk", "q8_0"], ["-ctv", "q8_0"], ["-fa", "on"]] {
            assert!(a.windows(2).any(|w| w == pair), "{pair:?} in {a:?}");
        }
        // the load request beats the profile, and a plain context size beats a per-slot one set below it
        let load = Resolved::new((Some(16384), None, Some(2), &none), Some(&model), &all, (8192, 999, 2));
        assert_eq!((load.ctx_total, load.slots), (16384, 2));
        assert_eq!(load.ctx_per_slot(), 8192);
    }

    #[test]
    fn unset_options_add_no_flags_and_defaults_stay() {
        let r = Resolved::new(
            (None, None, None, &ServeFlags::default()),
            None,
            &ServeProfile::default(),
            (8192, 999, 2),
        );
        assert_eq!(r.args(), ["-c", "8192", "-ngl", "999", "-np", "2"]);
        r.validate().unwrap();
    }

    #[test]
    fn every_flag_is_built() {
        let f = ServeFlags {
            ctx_per_slot: None,
            cache_type_k: Some("q8_0".into()),
            cache_type_v: Some("q4_0".into()),
            flash_attn: Some("on".into()),
            cache_prompt: Some(false),
            cache_reuse: None,
            cache_ram_mib: Some(-1),
            kv_unified: Some(true),
            batch_size: Some(2048),
            ubatch_size: Some(1024),
            jinja: Some(true),
            chat_template_file: None,
        };
        let r = Resolved::new(
            (Some(65536), Some(20), Some(3), &f),
            None,
            &ServeProfile::default(),
            (1, 1, 1),
        );
        let a = r.args().join(" ");
        assert_eq!(
            a,
            "-c 65536 -ngl 20 -np 3 -ctk q8_0 -ctv q4_0 -fa on --no-cache-prompt --cache-ram -1 --kv-unified -b 2048 -ub 1024 --jinja"
        );
        let g = ServeFlags {
            cache_prompt: Some(true),
            cache_reuse: Some(256),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &g), None, &ServeProfile::default(), (1, 1, 1));
        assert!(r.args().join(" ").ends_with("--cache-prompt --cache-reuse 256"));
    }

    #[test]
    fn bad_values_are_refused_with_the_reason() {
        let bad = |f: ServeFlags| validate_flags(&f, Some(1)).unwrap_err();
        assert!(bad(ServeFlags {
            cache_type_k: Some("q9".into()),
            ..Default::default()
        })
        .contains("cache_type_k"));
        assert!(bad(ServeFlags {
            cache_type_v: Some("q8_0".into()),
            flash_attn: Some("off".into()),
            ..Default::default()
        })
        .contains("needs flash attention"));
        assert!(bad(ServeFlags {
            flash_attn: Some("yes".into()),
            ..Default::default()
        })
        .contains("flash_attn"));
        assert!(bad(ServeFlags {
            batch_size: Some(256),
            ubatch_size: Some(512),
            ..Default::default()
        })
        .contains("ubatch_size"));
        assert!(bad(ServeFlags {
            cache_reuse: Some(8),
            cache_prompt: Some(false),
            ..Default::default()
        })
        .contains("cache_prompt"));
        assert!(bad(ServeFlags {
            chat_template_file: Some("/no/such/template.jinja".into()),
            ..Default::default()
        })
        .contains("chat_template_file"));
        assert!(validate_flags(&ServeFlags::default(), Some(0)).is_err());
        assert!(validate_flags(
            &ServeFlags {
                cache_type_v: Some("q8_0".into()),
                flash_attn: Some("auto".into()),
                ..Default::default()
            },
            Some(4)
        )
        .is_ok());
    }

    #[test]
    fn profiles_parse_from_toml_and_a_misspelt_key_is_found() {
        #[derive(Deserialize, Serialize)]
        struct Cfg {
            #[serde(default)]
            profiles: BTreeMap<String, ServeProfile>,
        }
        let ok: Cfg = toml::from_str(
            "[profiles.\"coder\"]\nctx_per_slot = 32768\nparallel = 4\ncache_type_k = \"q8_0\"\nflash_attn = \"on\"\ncache_prompt = true\n",
        )
        .unwrap();
        let p = &ok.profiles["coder"];
        assert_eq!((p.parallel, p.flags.ctx_per_slot), (Some(4), Some(32768)));
        assert_eq!(p.flags.cache_type_k.as_deref(), Some("q8_0"));
        p.check("profile coder").unwrap();
        let back: Cfg = toml::from_str(&toml::to_string_pretty(&ok).unwrap()).unwrap();
        assert_eq!(back.profiles["coder"], *p);
        let typo: Cfg = toml::from_str("[profiles.m]\ncache_tyep_k = \"q8_0\"\n").unwrap();
        let e = typo.profiles["m"].check("profile m").unwrap_err();
        assert!(e.contains("cache_tyep_k"), "{e}");
    }
}

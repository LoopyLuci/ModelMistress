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
    /// The engine that serves the model: "llama.cpp" (the default) or "rigel" (Rigel, a build of a
    /// llama.cpp fork for Windows and AMD GPUs, which can page a mixture-of-experts model's experts from disk).
    /// Rigel serves one request at a time: its loads always get one slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Rigel: keep the routed experts off the GPU and stream them on demand from the GGUF or the expert pack into a
    /// pool of VRAM slots (`--moe-direct-io`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_direct_io: Option<bool>,
    /// Rigel: VRAM expert slots in all, shared by the layers (`--moe-slots`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_slots: Option<u32>,
    /// Rigel: the expert pack folder, with `experts.bin` (`--moe-pack-dir`); stripped bundles need it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_pack_dir: Option<PathBuf>,
    /// Rigel: a hot-expert profile that seeds the VRAM slots (`--moe-profile`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_profile: Option<PathBuf>,
    /// Rigel: pinned host memory (MiB) that keeps evicted experts (the `MOE_CACHE_MIB` variable); 0 = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moe_cache_mib: Option<u32>,
    /// Rigel: dense layers to read ahead from the memory-mapped file while the current layer computes
    /// (`--layer-readahead`; 0 = off). For a dense model bigger than the free memory, whose weights the operating system
    /// pages in: the next layers are read in large ordered requests instead of one page fault at a time. Costs a few
    /// percent on a model that fits, so leave it off for those.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_readahead: Option<u32>,
    /// Rigel: MiB of dense layer weights to lock in memory so the operating system never evicts them, the rest being
    /// streamed from the memory-mapped file (`--layer-resident-mib`; -1 = what is free minus a margin, 0 = off). For a
    /// dense model bigger than the free memory: a walk over every layer for every token makes least-recently-used
    /// eviction throw out each page just before it is needed, so the whole model is read from disk for every token;
    /// with the first layers locked only the rest is. Works with `layer_readahead`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_resident_mib: Option<i32>,
    /// Allow this model to be loaded although it is bigger than the free RAM plus the VRAM models may use: the weights
    /// that do not fit are read from disk by the operating system as the model runs (llama.cpp memory-maps the file).
    /// Slow: every token may read from disk. It also needs the hub's `allow_paged_load` setting, and the model is only
    /// ever loaded on purpose (`model.load`), never by a request, Misty or `auto`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paged: Option<bool>,
}

/// The engines a model can be served by (`engine` in a serving profile).
pub const ENGINES: [&str; 3] = ["llama.cpp", "rigel", "prism"];

impl ServeFlags {
    /// Serve with Rigel (`engine = "rigel"`).
    pub fn is_rigel(&self) -> bool {
        self.engine.as_deref() == Some("rigel")
    }

    /// Serve with the Prism llama-server (`engine = "prism"`).
    pub fn is_prism(&self) -> bool {
        self.engine.as_deref() == Some("prism")
    }

    /// The Rigel-only options that are set, by name.
    pub fn rigel_options_set(&self) -> Vec<&'static str> {
        let mut v = vec![];
        if self.moe_direct_io.is_some() {
            v.push("moe_direct_io");
        }
        if self.moe_slots.is_some() {
            v.push("moe_slots");
        }
        if self.moe_pack_dir.is_some() {
            v.push("moe_pack_dir");
        }
        if self.moe_profile.is_some() {
            v.push("moe_profile");
        }
        if self.moe_cache_mib.is_some() {
            v.push("moe_cache_mib");
        }
        if self.layer_readahead.is_some() {
            v.push("layer_readahead");
        }
        if self.layer_resident_mib.is_some() {
            v.push("layer_resident_mib");
        }
        v
    }
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
            engine: self.engine.clone().or_else(|| base.engine.clone()),
            moe_direct_io: self.moe_direct_io.or(base.moe_direct_io),
            moe_slots: self.moe_slots.or(base.moe_slots),
            moe_pack_dir: self.moe_pack_dir.clone().or_else(|| base.moe_pack_dir.clone()),
            moe_profile: self.moe_profile.clone().or_else(|| base.moe_profile.clone()),
            moe_cache_mib: self.moe_cache_mib.or(base.moe_cache_mib),
            layer_readahead: self.layer_readahead.or(base.layer_readahead),
            layer_resident_mib: self.layer_resident_mib.or(base.layer_resident_mib),
            paged: self.paged.or(base.paged),
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
        if f.engine.as_deref().is_some_and(|e| !ENGINES.contains(&e)) {
            let e = f.engine.take().unwrap_or_default();
            drop("engine", format!("\"{e}\" is not one of {}", ENGINES.join(", ")));
        }
        if f.moe_slots == Some(0) {
            f.moe_slots = None;
            drop("moe_slots", "must be at least 1".into());
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
        if f.layer_resident_mib.is_some_and(|n| n < -1) {
            f.layer_resident_mib = None;
            drop(
                "layer_resident_mib",
                "must be -1 (what is free), 0 (off) or a size in MiB".into(),
            );
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
    if let Some(e) = &f.engine {
        if !ENGINES.contains(&e.as_str()) {
            return Err(format!("engine \"{e}\" is not one of {}", ENGINES.join(", ")));
        }
    }
    if f.moe_slots == Some(0) {
        return Err("moe_slots must be at least 1".into());
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
    if f.layer_resident_mib.is_some_and(|n| n < -1) {
        return Err("layer_resident_mib must be -1 (what is free), 0 (off) or a size in MiB".into());
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
    /// Set when llama-server is to choose how many layers go to the GPU (`--fit on`, with this many MiB left free on
    /// each device) instead of being told with `-ngl`. Only for a `gpu_layers` that means "as many as fit".
    pub fit_target_mib: Option<u64>,
}

/// `gpu_layers` values that mean "as many layers as fit": the default 999 (all of them) and a negative number.
pub fn is_auto_layers(n: i32) -> bool {
    !(0..999).contains(&n)
}

impl Resolved {
    /// Whether the number of GPU layers is the hub's and llama-server's to choose.
    pub fn is_auto(&self) -> bool {
        is_auto_layers(self.gpu_layers)
    }

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
        let gpu_layers = ngl
            .or_else(|| model.and_then(|p| p.gpu_layers))
            .or(all.gpu_layers)
            .unwrap_or(legacy.1);
        let mut merged = flags.clone();
        if let Some(m) = model {
            merged = merged.over(&m.flags);
        }
        merged = merged.over(&all.flags);
        // Rigel has one paged context per process: one slot, whatever `parallel` says.
        let slots = if merged.is_rigel() {
            1
        } else {
            pick(par, |p| p.parallel, legacy.2).max(1)
        };
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
            fit_target_mib: None,
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
        validate_flags(&self.flags, Some(self.slots))?;
        // Checked on the merged result: the engine and the Rigel options may come from different layers.
        if !self.flags.is_rigel() {
            if let Some(k) = self.flags.rigel_options_set().first() {
                return Err(format!("{k} is a Rigel option: it needs engine = \"rigel\""));
            }
        }
        Ok(())
    }

    /// Environment variables for the server process (Rigel's host cache size).
    pub fn env(&self) -> Vec<(String, String)> {
        match self.flags.moe_cache_mib {
            Some(n) if self.flags.is_rigel() => vec![("MOE_CACHE_MIB".into(), n.to_string())],
            _ => vec![],
        }
    }

    /// The llama-server arguments. `-c`, `-ngl` (or `--fit`, see `fit_target_mib`) and `-np` always; the rest only for
    /// what is set.
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-c".into(), self.ctx_total.to_string()];
        match self.fit_target_mib.filter(|_| self.is_auto()) {
            Some(target) => a.extend(["--fit".into(), "on".into(), "--fit-target".into(), target.to_string()]),
            None => a.extend(["-ngl".into(), self.gpu_layers.to_string()]),
        }
        a.extend(["-np".into(), self.slots.to_string()]);
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
        if f.is_rigel() {
            if f.moe_direct_io == Some(true) {
                a.push("--moe-direct-io".into());
            }
            if let Some(n) = f.moe_slots {
                a.extend(["--moe-slots".into(), n.to_string()]);
            }
            if let Some(p) = &f.moe_pack_dir {
                a.extend(["--moe-pack-dir".into(), p.display().to_string()]);
            }
            if let Some(p) = &f.moe_profile {
                a.extend(["--moe-profile".into(), p.display().to_string()]);
            }
            if let Some(n) = f.layer_readahead.filter(|n| *n > 0) {
                a.extend(["--layer-readahead".into(), n.to_string()]);
            }
            if let Some(n) = f.layer_resident_mib.filter(|n| *n != 0) {
                a.extend(["--layer-resident-mib".into(), n.to_string()]);
            }
        }
        a
    }
}

/// What a loaded model reports about how it is served (`model.loaded`, `/v1/models`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Serving {
    /// The engine serving the model: "llama.cpp" or "rigel".
    pub engine: &'static str,
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
    /// Where the weights went: "gpu" (all on the GPU), "partial" (some layers on the GPU, the rest on the CPU), "cpu",
    /// or "unknown" (not measured: no GPU counters, or not loaded yet).
    pub placement: &'static str,
    /// What the server's own log says about the layers: how many it put on the GPU, the buffers on each side, the
    /// graph splits. This decides `placement` when it is there (the server prints it at verbosity 4, which the hub
    /// asks for).
    pub layers: Option<crate::loadlog::LayerSummary>,
    /// Why it is where it is, when that is not what was asked for (a retry on the CPU after the GPU failed, a
    /// remembered failure).
    pub placement_note: Option<String>,
    /// Where `n_ctx` came from: "server" (asked /props) or "requested".
    pub source: &'static str,
}

impl Serving {
    pub fn from_resolved(r: &Resolved) -> Serving {
        let unified = r.flags.kv_unified.unwrap_or(false);
        Serving {
            engine: if r.flags.is_rigel() {
                "rigel"
            } else if r.flags.is_prism() {
                "prism"
            } else {
                "llama.cpp"
            },
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
            placement: if r.gpu_layers == 0 { "cpu" } else { "unknown" },
            layers: None,
            placement_note: None,
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

/// What one token adds to the cache of the layers of one kind: the elements of K and of V, summed over those layers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KvElems {
    pub k: u64,
    pub v: u64,
}

impl KvElems {
    fn is_empty(&self) -> bool {
        self.k == 0 && self.v == 0
    }
}

/// What the planner needs to know about a model, read from its GGUF header.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelShape {
    pub file_bytes: u64,
    /// Transformer layers that are served (the multi-token-prediction layer a file may carry is left out).
    pub layers: u64,
    /// Of those, the ones with a KV cache (a hybrid model has attention in every Nth layer only, and a model that shares
    /// the cache of its last layers has none there).
    pub attn_layers: u64,
    pub n_embd: u64,
    /// Per token, over the layers that attend over the whole context.
    pub kv_full: KvElems,
    /// Per token, over the layers that attend over a sliding window. Their cache holds the window (and a batch), not the
    /// context: that is most of the layers of a Gemma, and why a long context costs so little there.
    pub kv_swa: KvElems,
    /// Tokens of the window (0: no layer has one).
    pub swa_window: u64,
    /// Bytes of recurrent state one slot needs (0 for a pure transformer).
    pub state_bytes_per_slot: u64,
    /// Fraction of the weights that are expert tensors (0 for a dense model).
    pub expert_share: f64,
}

fn meta_u64(meta: &Map<String, Value>, key: &str) -> Option<u64> {
    meta.get(key).and_then(Value::as_u64)
}

/// A setting that may be one number or one per layer (`head_count_kv` of a model whose layers differ): the number, or
/// the largest of the list.
fn meta_num(meta: &Map<String, Value>, key: &str) -> Option<u64> {
    match meta.get(key)? {
        Value::Array(a) => a.iter().filter_map(Value::as_u64).max(),
        v => v.as_u64(),
    }
}

/// One number per layer, when the file lists them.
fn meta_per_layer(meta: &Map<String, Value>, key: &str) -> Option<Vec<u64>> {
    let a = meta.get(key)?.as_array()?;
    Some(a.iter().map(|v| v.as_u64().unwrap_or(0)).collect())
}

/// How llama.cpp builds the sliding-window pattern of the architectures that have one built in (`load_swa_pattern`
/// in each model's loader): the period (every Nth layer attends over the whole context, the others over the window),
/// whether the first layer of a period is the full one, and the window when the file does not give one.
const SWA_BUILT_IN: &[(&str, u64, bool, u64)] = &[
    ("gemma2", 2, false, 4096),
    ("gemma3", 6, false, 0),
    ("gemma3n", 5, false, 0),
    ("cohere2", 4, false, 0),
    ("gpt-oss", 2, false, 0),
    ("olmo2", 4, false, 0),
    ("afmoe", 4, false, 0),
    ("plamo3", 8, false, 0),
];

/// Which of the first `n` layers attend over a window only, and how wide it is. The file's own list wins
/// (`sliding_window_pattern`, one flag per layer: Gemma 4), then its period, then the one built into llama.cpp for the
/// architecture. A model with none of them (or no window) is taken as all full attention, which is the safe side.
fn swa_layers(meta: &Map<String, Value>, arch: &str, n: u64) -> (Vec<bool>, u64) {
    let built_in = SWA_BUILT_IN.iter().find(|(a, ..)| *a == arch);
    let window = meta_u64(meta, &format!("{arch}.attention.sliding_window"))
        .filter(|w| *w > 0)
        .or_else(|| built_in.map(|b| b.3).filter(|w| *w > 0));
    let Some(window) = window else {
        return (vec![false; n as usize], 0);
    };
    let by_period = |period: u64, dense_first: bool| -> Vec<bool> {
        (0..n)
            .map(|il| {
                period == 0
                    || if dense_first {
                        il % period != 0
                    } else {
                        il % period < period - 1
                    }
            })
            .collect()
    };
    let flags = match meta.get(&format!("{arch}.attention.sliding_window_pattern")) {
        Some(Value::Array(a)) => (0..n as usize)
            .map(|i| match a.get(i) {
                Some(Value::Bool(b)) => *b,
                Some(v) => v.as_u64().unwrap_or(0) != 0,
                None => false,
            })
            .collect(),
        Some(v) if v.as_u64().is_some() => by_period(v.as_u64().unwrap_or(0), false),
        _ => match built_in {
            Some((_, period, dense_first, _)) => by_period(*period, *dense_first),
            None => vec![false; n as usize],
        },
    };
    if flags.iter().any(|f| *f) {
        (flags, window)
    } else {
        (flags, 0)
    }
}

/// The shape from the scalar GGUF metadata (`<arch>.*`). None when the layers or widths are not in it.
pub fn shape_from_meta(meta: &Map<String, Value>, file_bytes: u64) -> Option<ModelShape> {
    let arch = meta.get("general.architecture")?.as_str()?;
    let k = |s: &str| meta_u64(meta, &format!("{arch}.{s}"));
    let kn = |s: &str| meta_num(meta, &format!("{arch}.{s}"));
    let blocks = k("block_count")?;
    let nextn = k("nextn_predict_layers").unwrap_or(0);
    let layers = blocks.saturating_sub(nextn).max(1);
    let n_embd = k("embedding_length")?;
    let n_head = kn("attention.head_count")?.max(1);
    let n_head_kv = kn("attention.head_count_kv").unwrap_or(n_head);
    let key_len = k("attention.key_length").unwrap_or(n_embd / n_head);
    let val_len = k("attention.value_length").unwrap_or(key_len);
    let key_len_swa = k("attention.key_length_swa").unwrap_or(key_len);
    let val_len_swa = k("attention.value_length_swa").unwrap_or(val_len);
    let interval = k("full_attention_interval").filter(|n| *n > 1);
    // the layers that keep a cache of their own: the first ones, when the last share the cache of earlier layers
    let with_cache = match arch {
        "gemma3n" => layers.min(20),
        _ => layers
            .saturating_sub(k("attention.shared_kv_layers").unwrap_or(0))
            .max(1),
    };
    let heads_kv = meta_per_layer(meta, &format!("{arch}.attention.head_count_kv"));
    let (swa, window) = swa_layers(meta, arch, layers);
    let (mut kv_full, mut kv_swa) = (KvElems::default(), KvElems::default());
    let mut attn_layers = 0;
    for il in 0..layers {
        // a hybrid has attention in every Nth layer only; a list of heads per layer says it with zeros
        let attends = il < with_cache && interval.is_none_or(|n| (il + 1) % n == 0);
        let heads = match &heads_kv {
            Some(h) => h.get(il as usize).copied().unwrap_or(0),
            None => n_head_kv,
        };
        if !attends || heads == 0 {
            continue;
        }
        attn_layers += 1;
        let is_swa = swa[il as usize];
        let (kl, vl) = if is_swa {
            (key_len_swa, val_len_swa)
        } else {
            (key_len, val_len)
        };
        let into = if is_swa { &mut kv_swa } else { &mut kv_full };
        into.k += heads * kl;
        into.v += heads * vl;
    }
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
        kv_full,
        kv_swa,
        swa_window: if kv_swa.is_empty() { 0 } else { window },
        state_bytes_per_slot,
        expert_share,
    })
}

// Bytes one element of a KV type takes.
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
    /// `--kv-unified`: one cache for all the slots.
    pub kv_unified: bool,
    /// `--swa-full`: the sliding-window layers keep the whole context, as the others do.
    pub swa_full: bool,
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

/// Cells the cache of the sliding-window layers has, as llama.cpp sizes it (`llama_kv_cache_iswa`): the window plus a
/// batch, rounded up to 256, for each slot (or for all of them together, with a unified cache), and never more than the
/// context. With `--swa-full` it is the whole context.
fn swa_cells(window: u64, p: &Placement) -> u64 {
    if window == 0 || p.swa_full {
        return p.ctx_total;
    }
    let pad = |n: u64| n.div_ceil(256) * 256;
    let slots = p.slots.max(1);
    if p.kv_unified {
        pad(p.ctx_total.min(window * slots + p.ubatch))
    } else {
        slots * pad((p.ctx_total / slots).min(window + p.ubatch))
    }
}

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
    let per_token =
        |e: &KvElems| e.k as f64 * kv_bytes_per_elem(&p.cache_type_k) + e.v as f64 * kv_bytes_per_elem(&p.cache_type_v);
    let kv = (per_token(&shape.kv_full) * p.ctx_total as f64
        + per_token(&shape.kv_swa) * swa_cells(shape.swa_window, p) as f64)
        * on_gpu;
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
    // a model that holds no VRAM (it runs on the CPU) frees none: leave it loaded
    let mut idle: Vec<&Resident> = resident.iter().filter(|r| !r.busy && r.vram_mib > 0).collect();
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

/// Where a load of `est`'s size can go, for a model whose number of GPU layers is for the hub and llama-server to
/// choose (`gpu_layers` 999: every layer that fits).
#[derive(Debug, Clone, PartialEq)]
pub enum Fit {
    /// Every layer fits on the GPU as it is.
    Gpu,
    /// Every layer fits after these idle models are unloaded (the MiB to be free is the second field).
    GpuAfter(Vec<String>, u64),
    /// Part of it fits: the GPU takes that share, the CPU the rest.
    Split { gpu_share: f64 },
    /// Next to nothing fits on the GPU: it runs on the CPU.
    Cpu,
    /// The CPU's part does not fit in the memory that is available either.
    No { need_ram_mib: u64, free_ram_mib: u64 },
    /// More than the RAM and the GPU hold, and paging was allowed: `paged_mib` of the weights are read from disk as the
    /// model runs (slow: expect seconds per token). What cannot be paged (the KV cache, the state, the server's buffers)
    /// does fit in RAM. `need_ram_mib` is what the load would need if it all fitted.
    Paged { paged_mib: u64, need_ram_mib: u64 },
}

/// Memory the server program itself and its buffers take on the CPU side, beyond the model.
pub const RAM_OVERHEAD_MIB: u64 = 768;

/// Chooses between the GPU, a split and the CPU. Unlike [`plan`] it never refuses for want of VRAM alone: what does not
/// fit on the GPU runs from RAM, and only a model that fits neither is turned down.
pub fn place(est: &VramEstimate, free_vram_mib: u64, margin_mib: u64, resident: &[Resident], free_ram_mib: u64) -> Fit {
    place_with(est, free_vram_mib, margin_mib, resident, free_ram_mib, false)
}

/// [`place`], where a model that fits neither the GPU nor RAM may be placed as [`Fit::Paged`] when `allow_paged`.
pub fn place_with(
    est: &VramEstimate,
    free_vram_mib: u64,
    margin_mib: u64,
    resident: &[Resident],
    free_ram_mib: u64,
    allow_paged: bool,
) -> Fit {
    match plan(est.total_mib, free_vram_mib, margin_mib, resident) {
        Plan::Fits => Fit::Gpu,
        Plan::Evict(models) => Fit::GpuAfter(models, est.total_mib + margin_mib),
        Plan::Refuse { .. } => {
            let movable = (est.weights_mib + est.kv_mib + est.state_mib).max(1);
            let usable = free_vram_mib.saturating_sub(margin_mib + est.compute_mib);
            let on_gpu = usable.min(movable);
            let need_ram_mib = movable - on_gpu + RAM_OVERHEAD_MIB;
            // the weights can be paged from disk; the cache, the state and the server's own buffers cannot
            let fixed_mib = est.kv_mib + est.state_mib + RAM_OVERHEAD_MIB;
            if need_ram_mib > free_ram_mib && allow_paged && fixed_mib <= free_ram_mib {
                Fit::Paged {
                    paged_mib: need_ram_mib - free_ram_mib,
                    need_ram_mib,
                }
            } else if need_ram_mib > free_ram_mib {
                Fit::No {
                    need_ram_mib,
                    free_ram_mib,
                }
            } else if (on_gpu as f64 / movable as f64) < 0.1 {
                Fit::Cpu
            } else {
                Fit::Split {
                    gpu_share: on_gpu as f64 / movable as f64,
                }
            }
        }
    }
}

/// Layers a load that was to run on the CPU should put on the card because its weights do not fit the free RAM: the
/// fewest that leave the rest in RAM, and no more than the free VRAM holds (what still does not fit is read from disk, as
/// a CPU load always was). 0 when the file fits in RAM, or when the card has no room for even one layer. `est` is the
/// estimate with every layer on the card (weights, cache, state and the compute buffers, which a card load needs once).
pub fn spill_layers(
    est: &VramEstimate,
    layers: u64,
    file_mib: u64,
    free_ram_mib: u64,
    free_vram_mib: u64,
    margin_mib: u64,
) -> u64 {
    let need_ram = file_mib + RAM_OVERHEAD_MIB;
    if layers == 0 || need_ram <= free_ram_mib {
        return 0;
    }
    let ram_per_layer = (file_mib / layers).max(1);
    let wanted = (need_ram - free_ram_mib).div_ceil(ram_per_layer).min(layers);
    let movable = est.weights_mib + est.kv_mib + est.state_mib;
    let vram_per_layer = movable.div_ceil(layers).max(1);
    let usable = free_vram_mib.saturating_sub(margin_mib + est.compute_mib);
    wanted.min(usable / vram_per_layer)
}

/// VRAM expert slots (`--moe-slots`) a paged Rigel load of a mixture-of-experts model can afford in `budget_mib`: what is
/// left after the non-expert weights, the cache and the compute buffers (`paged`: the estimate with the experts left out),
/// at 90 %, divided by the size of one expert slot. Measured on a 22 GB Q8_0 MoE: 1536 slots 19 GB and 65.7 tokens/s,
/// 1024 slots 14.4 GB and 35.8, 512 slots 10.6 GB and 21.6. None when fewer than 64 fit (paging would thrash), and never
/// more than the model has experts (then nothing needs paging).
pub fn paged_slots(shape: &ModelShape, expert_count: u64, paged: &VramEstimate, budget_mib: u64) -> Option<u64> {
    let all = shape.layers.checked_mul(expert_count)?;
    if all == 0 || shape.expert_share <= 0.0 {
        return None;
    }
    let slot_mib = shape.file_bytes as f64 / MIB * shape.expert_share / all as f64;
    let room = budget_mib.saturating_sub(paged.total_mib) as f64 * 0.9;
    let slots = (room / slot_mib.max(0.01)) as u64;
    (slots >= 64).then_some(slots.min(all))
}

/// A JSON view of a placement for the API (`model.plan`).
pub fn fit_json(f: &Fit) -> Value {
    match f {
        Fit::Gpu => json!({"where": "gpu"}),
        Fit::GpuAfter(models, want) => {
            json!({"where": "gpu_after_unloading", "unload": models, "free_mib_needed": want})
        }
        Fit::Split { gpu_share } => json!({"where": "split", "gpu_share": (gpu_share * 100.0).round() / 100.0}),
        Fit::Cpu => json!({"where": "cpu"}),
        Fit::No {
            need_ram_mib,
            free_ram_mib,
        } => json!({"where": "none", "need_ram_mib": need_ram_mib, "free_ram_mib": free_ram_mib}),
        Fit::Paged {
            paged_mib,
            need_ram_mib,
        } => json!({
            "where": "paged", "paged_mib": paged_mib, "need_ram_mib": need_ram_mib,
            "note": "disk-bound: part of the weights is read from disk as the model runs; expect seconds per token"}),
    }
}

// ---- what a card really lets one process hold ------------------------------------------------------------------

/// What loads have shown about how much of a card one process can hold. The driver's free figure is optimistic, and on
/// Windows device memory is also charged against the system's commit (see `machine`), so a load that "fits" the card
/// can still run out of memory. Measured on an RX 7900 XTX: 23.4 GB reported free, a 20 GB load failed on a 1 GB buffer
/// while 25 GB of commit headroom was left, and the same load worked a while later. The hub keeps the smallest load
/// that failed (for a day: what else holds the memory changes) and the largest that worked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuLimit {
    /// The most dedicated memory (MiB) a model server has held there after it loaded.
    #[serde(default)]
    pub ok_mib: u64,
    /// The smallest ask (MiB) that ran out of memory there. 0: none has, or it was lifted.
    #[serde(default)]
    pub failed_mib: u64,
    /// When that failure was seen (seconds since the epoch).
    #[serde(default)]
    pub failed_at: u64,
}

/// How long a failure is believed: what else is on the machine changes in a day.
pub const LIMIT_TTL_S: u64 = 24 * 3600;
/// The least a card is asked to hold in a split load. Below it the split is not worth making.
pub const MIN_SPLIT_MIB: u64 = 1536;
/// How many split loads are tried, each held further below the last, before the CPU.
pub const MAX_SPLIT_TRIES: usize = 3;

impl GpuLimit {
    /// Whether a failure is on record and still believed.
    pub fn failure_is_live(&self, now: u64) -> bool {
        self.failed_mib > 0 && now.saturating_sub(self.failed_at) <= LIMIT_TTL_S
    }

    /// A load that asked `mib` of the card ran out of memory. An ask at or below what has worked is no news about the
    /// card (something else held its memory at the time), and changes nothing. True when it was learned.
    pub fn failed(&mut self, mib: u64, now: u64) -> bool {
        if mib <= self.ok_mib || mib == 0 {
            return false;
        }
        self.failed_mib = if self.failure_is_live(now) {
            self.failed_mib.min(mib)
        } else {
            mib
        };
        self.failed_at = now;
        true
    }

    /// A server loaded and holds `mib`. A load that held what once failed means the limit is gone. True when this
    /// changed anything.
    pub fn worked(&mut self, mib: u64) -> bool {
        let mut changed = false;
        if mib > self.ok_mib {
            self.ok_mib = mib;
            changed = true;
        }
        if self.failed_mib != 0 && mib >= self.failed_mib {
            self.failed_mib = 0;
            changed = true;
        }
        changed
    }

    /// What the next load is held to: an eighth under the ask that failed, and never less than what has worked. None:
    /// nothing has failed lately, so the driver's figure stands.
    pub fn budget_mib(&self, now: u64) -> Option<u64> {
        self.failure_is_live(now).then(|| {
            let under = self.failed_mib - self.failed_mib / 8;
            under.max(self.ok_mib).min(self.failed_mib - 1)
        })
    }
}

/// The `--fit-target` (MiB that llama-server keeps free on each card) that holds a load to `budget_mib` of a card with
/// `free_mib` free, and never leaves less than `floor_mib` free.
pub fn fit_target_for(free_mib: u64, budget_mib: u64, floor_mib: u64) -> u64 {
    free_mib.saturating_sub(budget_mib).max(floor_mib)
}

/// The free VRAM a load is planned against: what the cards have free, held to what models may use of them, which is
/// each card's total less `reserve_mib` (kept for the system) and less what the hub's other models already hold. The
/// margin is added back to the cap, because [`plan`] asks for the margin on top of a load's need: the reserve is a limit
/// on what models use, and the margin is slack for the estimate.
/// Threads (`-t`) for one llama-server that is about to start, so that a model on the CPU and a model on the GPU work
/// at the same time without fighting for the cores. A server whose model sits wholly on the card only feeds the card
/// from one thread: it gets a few (a third of the physical cores, between 2 and 4). A CPU-only server gets the cores
/// the GPU servers leave, shared with the CPU servers already running. `None` leaves llama-server's own default (all
/// physical cores): a machine with fewer than 4 cores, a model split between card and RAM, a CPU model with nothing
/// else running.
pub fn thread_budget(
    cores: usize,
    uses_gpu: bool,
    wholly_on_gpu: bool,
    gpu_servers: usize,
    cpu_servers: usize,
) -> Option<u32> {
    if cores < 4 {
        return None;
    }
    let host = (cores / 3).clamp(2, 4);
    if uses_gpu {
        return wholly_on_gpu.then_some(host as u32);
    }
    if gpu_servers == 0 && cpu_servers == 0 {
        return None;
    }
    let free = cores.saturating_sub(host * gpu_servers);
    Some((free / (cpu_servers + 1)).max(2) as u32)
}

/// How far a load has come, from what the server has read of its file: `stage` says what it is doing, and the rate and
/// the time left are what it has read over the time it has had. A model on a hard drive takes minutes to load and
/// shows nothing in that time otherwise.
pub fn load_progress(size_mib: u64, read_mib: u64, elapsed_s: u64) -> serde_json::Value {
    let read = read_mib.min(size_mib);
    let percent = (read * 100).checked_div(size_mib).unwrap_or(0).min(100);
    let rate = if elapsed_s == 0 {
        0.0
    } else {
        read as f64 / elapsed_s as f64
    };
    let stage = if read * 100 < size_mib {
        "starting"
    } else if percent < 98 {
        "reading from disk"
    } else {
        "preparing"
    };
    let eta = (rate >= 1.0 && percent < 100).then(|| ((size_mib - read) as f64 / rate).ceil() as u64);
    serde_json::json!({
        "stage": stage, "percent": percent, "read_mib": read, "size_mib": size_mib,
        "rate_mib_s": (rate * 10.0).round() / 10.0, "eta_s": eta,
    })
}

/// Whether these llama-server arguments already say how many threads to use.
pub fn sets_threads(args: &[String]) -> bool {
    args.iter()
        .any(|a| matches!(a.as_str(), "-t" | "--threads" | "-tb" | "--threads-batch") || a.starts_with("--threads="))
}

pub fn plannable_free_mib(devices: &[Device], reserve_mib: u64, held_by_others_mib: u64, margin_mib: u64) -> u64 {
    let free: u64 = devices.iter().map(|d| d.free_mib).sum();
    let cap: u64 = devices.iter().map(|d| d.total_mib.saturating_sub(reserve_mib)).sum();
    free.min(cap.saturating_sub(held_by_others_mib).saturating_add(margin_mib))
}

/// What a load asks of a card: its need, or what its `--fit-target` leaves of the free memory if that is less.
pub fn asked_of_card(need_mib: u64, free_mib: u64, target_mib: u64) -> u64 {
    need_mib.min(free_mib.saturating_sub(target_mib))
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

/// Where a loaded model's weights are. `vram_mib` is the GPU memory its server holds (None: none was found), `counted`
/// says whether the machine's GPU counters work (then "none found" means none is held), `full_mib` is the weights, KV
/// cache and state of a load of every layer onto the GPU (what moves with the layers), and `layers` the model's layer
/// count.
pub fn placement_of(
    vram_mib: Option<f64>,
    counted: bool,
    full_mib: Option<u64>,
    gpu_layers: i32,
    layers: Option<u64>,
) -> &'static str {
    if gpu_layers == 0 {
        return "cpu";
    }
    let Some(v) = vram_mib else {
        return if counted { "cpu" } else { "unknown" };
    };
    if v < 128.0 {
        return "cpu";
    }
    if !is_auto_layers(gpu_layers) {
        return match layers {
            Some(l) if (gpu_layers as u64) < l => "partial",
            _ => "gpu",
        };
    }
    match full_mib {
        Some(f) if f > 0 && v < f as f64 * 0.85 => "partial",
        _ => "gpu",
    }
}

/// A one-line JSON view of the estimate for the API.
pub fn estimate_json(e: &VramEstimate) -> Value {
    json!({"weights_mib": e.weights_mib, "kv_mib": e.kv_mib, "state_mib": e.state_mib,
           "compute_mib": e.compute_mib, "total_mib": e.total_mib})
}

#[cfg(test)]
mod tests {
    #[test]
    fn threads_are_split_between_the_cpu_and_the_gpu_servers() {
        use super::thread_budget as tb;
        // a model wholly on the card needs a few threads to feed it, not every core
        assert_eq!(tb(12, true, true, 0, 0), Some(4));
        assert_eq!(tb(6, true, true, 0, 0), Some(2));
        // a model split with RAM does its share of the work on the CPU: llama-server's default stands
        assert_eq!(tb(12, true, false, 1, 0), None);
        // a CPU model alone uses every core; next to a GPU server it gets what that leaves
        assert_eq!(tb(12, false, false, 0, 0), None);
        assert_eq!(tb(12, false, false, 1, 0), Some(8));
        assert_eq!(tb(12, false, false, 1, 1), Some(4));
        assert_eq!(tb(12, false, false, 0, 1), Some(6));
        // never fewer than two, and small machines are left alone
        assert_eq!(tb(8, false, false, 2, 3), Some(2));
        assert_eq!(tb(2, true, true, 0, 0), None);
    }

    #[test]
    fn a_load_reports_how_much_of_the_file_it_has_read() {
        use super::load_progress as lp;
        let p = lp(20_000, 0, 3);
        assert_eq!(p["stage"], "starting");
        assert!(p["eta_s"].is_null());
        // 5,000 of 20,000 MiB in 50 s: 100 MiB/s, 150 s to go
        let p = lp(20_000, 5_000, 50);
        assert_eq!(p["stage"], "reading from disk");
        assert_eq!(p["percent"], 25);
        assert_eq!(p["rate_mib_s"], 100.0);
        assert_eq!(p["eta_s"], 150);
        // the working set can exceed the file (buffers): never above 100 %
        let p = lp(1_000, 1_400, 10);
        assert_eq!(p["percent"], 100);
        assert_eq!(p["stage"], "preparing");
        assert!(p["eta_s"].is_null());
    }

    #[test]
    fn explicit_thread_arguments_are_noticed() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(super::sets_threads(&a(&["-t", "6"])));
        assert!(super::sets_threads(&a(&["--threads=6"])));
        assert!(super::sets_threads(&a(&["-ngl", "9", "-tb", "8"])));
        assert!(!super::sets_threads(&a(&["-ngl", "9", "-c", "4096"])));
    }
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
            kv_unified: false,
            swa_full: false,
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

    /// The header of gemma-4-31B-it as the file has it: 60 layers, every sixth one full attention (4 KV heads of 512),
    /// the rest a 1024-token window (16 KV heads of 256).
    fn gemma4_meta() -> Map<String, Value> {
        let heads: Vec<u64> = (0..60).map(|il| if il % 6 == 5 { 4 } else { 16 }).collect();
        let pattern: Vec<bool> = (0..60).map(|il| il % 6 != 5).collect();
        meta(&[
            ("general.architecture", json!("gemma4")),
            ("gemma4.block_count", json!(60)),
            ("gemma4.embedding_length", json!(5376)),
            ("gemma4.attention.head_count", json!(32)),
            ("gemma4.attention.head_count_kv", json!(heads)),
            ("gemma4.attention.key_length", json!(512)),
            ("gemma4.attention.value_length", json!(512)),
            ("gemma4.attention.key_length_swa", json!(256)),
            ("gemma4.attention.value_length_swa", json!(256)),
            ("gemma4.attention.sliding_window", json!(1024)),
            ("gemma4.attention.sliding_window_pattern", json!(pattern)),
            ("gemma4.attention.shared_kv_layers", json!(0)),
        ])
    }

    #[test]
    fn a_sliding_window_model_is_sized_as_the_server_sizes_it() {
        let shape = shape_from_meta(&gemma4_meta(), 11_213 << 20).unwrap();
        assert_eq!((shape.layers, shape.attn_layers, shape.swa_window), (60, 60, 1024));
        // 10 full layers x 4 heads x 512, 50 window layers x 16 heads x 256
        assert_eq!(shape.kv_full, KvElems { k: 20_480, v: 20_480 });
        assert_eq!(shape.kv_swa, KvElems { k: 204_800, v: 204_800 });
        // what llama-server printed for this file at -c 4096, one slot, f16: 4096 cells over 10 layers (320 MiB) and
        // 1536 cells over 50 (1200 MiB): the window, 1024, and a batch of 512
        let e = estimate_vram(&shape, &place(4096, 1, "f16", "f16"));
        assert_eq!(e.kv_mib, 320 + 1200);
        // the context grows the full layers only: at 32768 the window layers still hold 1536 cells
        let long = estimate_vram(&shape, &place(32768, 1, "f16", "f16"));
        assert_eq!(long.kv_mib, 2560 + 1200);
        // treated as all full attention with 32 KV heads of 512 (what the estimate did before) the same load came to
        // 15 GiB of cache at 4096 tokens
        let all_full = ModelShape {
            kv_full: KvElems {
                k: 60 * 32 * 512,
                v: 60 * 32 * 512,
            },
            kv_swa: KvElems::default(),
            swa_window: 0,
            ..shape.clone()
        };
        assert_eq!(estimate_vram(&all_full, &place(4096, 1, "f16", "f16")).kv_mib, 15_360);
        // slots each have a window of their own; the full layers share the -c
        let two = estimate_vram(&shape, &place(8192, 2, "f16", "f16"));
        assert_eq!(two.kv_mib, 640 + 2400);
        // a unified cache has one window for all the slots: 2 x 1024 + 512, rounded to 2560 cells
        let unified = estimate_vram(
            &shape,
            &Placement {
                kv_unified: true,
                ..place(8192, 2, "f16", "f16")
            },
        );
        assert_eq!(unified.kv_mib, 640 + 2000);
        // --swa-full keeps the whole context in every layer
        let full = estimate_vram(
            &shape,
            &Placement {
                swa_full: true,
                ..place(4096, 1, "f16", "f16")
            },
        );
        assert_eq!(full.kv_mib, 320 + 3200);
        // a context shorter than the window needs no more than the context
        let short = estimate_vram(&shape, &place(512, 1, "f16", "f16"));
        assert_eq!(short.kv_mib, 40 + 400);
    }

    #[test]
    fn the_period_built_into_the_architecture_stands_in_for_a_pattern_the_file_lacks() {
        // gemma 3 27B: 62 layers, every sixth full (10), the rest a 1024 window (52); 16 KV heads of 128, scalar
        let m = meta(&[
            ("general.architecture", json!("gemma3")),
            ("gemma3.block_count", json!(62)),
            ("gemma3.embedding_length", json!(5376)),
            ("gemma3.attention.head_count", json!(32)),
            ("gemma3.attention.head_count_kv", json!(16)),
            ("gemma3.attention.key_length", json!(128)),
            ("gemma3.attention.value_length", json!(128)),
            ("gemma3.attention.sliding_window", json!(1024)),
        ]);
        let shape = shape_from_meta(&m, 16 << 30).unwrap();
        assert_eq!(shape.kv_full.k, 10 * 16 * 128);
        assert_eq!(shape.kv_swa.k, 52 * 16 * 128);
        assert_eq!(shape.swa_window, 1024);
        // no window in the file: gemma 3 is plain attention
        let mut plain = m.clone();
        plain.remove("gemma3.attention.sliding_window");
        let shape = shape_from_meta(&plain, 16 << 30).unwrap();
        assert_eq!((shape.kv_swa, shape.swa_window), (KvElems::default(), 0));
        assert_eq!(shape.kv_full.k, 62 * 16 * 128);
        // gemma 2 has a window of 4096 built in, on every other layer (the even ones)
        let g2 = meta(&[
            ("general.architecture", json!("gemma2")),
            ("gemma2.block_count", json!(42)),
            ("gemma2.embedding_length", json!(3584)),
            ("gemma2.attention.head_count", json!(16)),
            ("gemma2.attention.head_count_kv", json!(8)),
            ("gemma2.attention.key_length", json!(256)),
            ("gemma2.attention.value_length", json!(256)),
        ]);
        let shape = shape_from_meta(&g2, 5 << 30).unwrap();
        assert_eq!(
            (shape.kv_full.k, shape.kv_swa.k, shape.swa_window),
            (21 * 8 * 256, 21 * 8 * 256, 4096)
        );
        // the file's own period wins over the built-in one, and 0 means every layer
        let mut own = g2.clone();
        own.insert("gemma2.attention.sliding_window_pattern".into(), json!(3));
        let shape = shape_from_meta(&own, 5 << 30).unwrap();
        assert_eq!(shape.kv_full.k, 14 * 8 * 256);
        // an architecture the table does not know, with a window but no pattern: all full, the safe side
        let mut other = g2.clone();
        other.insert("general.architecture".into(), json!("newarch"));
        for key in g2.keys().filter(|k| k.starts_with("gemma2.")) {
            other.insert(key.replace("gemma2.", "newarch."), g2[key].clone());
        }
        other.insert("newarch.attention.sliding_window".into(), json!(4096));
        let shape = shape_from_meta(&other, 5 << 30).unwrap();
        assert_eq!((shape.kv_swa, shape.swa_window), (KvElems::default(), 0));
        assert_eq!(shape.kv_full.k, 42 * 8 * 256);
    }

    #[test]
    fn layers_that_share_the_cache_of_earlier_ones_add_none() {
        let mut m = gemma4_meta();
        m.insert("gemma4.attention.shared_kv_layers".into(), json!(20));
        let shape = shape_from_meta(&m, 5 << 30).unwrap();
        // the first 40 layers keep a cache: 6 full ones (il 5, 11, ... 35) and 34 window ones
        assert_eq!(shape.attn_layers, 40);
        assert_eq!(shape.kv_full.k, 6 * 4 * 512);
        assert_eq!(shape.kv_swa.k, 34 * 16 * 256);
    }

    #[test]
    fn a_list_of_heads_per_layer_drops_the_layers_without_attention() {
        // a hybrid that says it with zeros instead of an interval: attention in 2 of 4 layers
        let m = meta(&[
            ("general.architecture", json!("lst")),
            ("lst.block_count", json!(4)),
            ("lst.embedding_length", json!(1024)),
            ("lst.attention.head_count", json!(8)),
            ("lst.attention.head_count_kv", json!([0, 2, 0, 4])),
            ("lst.attention.key_length", json!(64)),
        ]);
        let shape = shape_from_meta(&m, 1 << 30).unwrap();
        assert_eq!(shape.attn_layers, 2);
        assert_eq!(shape.kv_full, KvElems { k: 6 * 64, v: 6 * 64 });
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
    fn a_card_that_ran_out_of_memory_is_held_below_the_load_that_failed_and_above_what_worked() {
        let mut l = GpuLimit::default();
        // nothing learned: the driver's figure stands
        assert_eq!(l.budget_mib(100), None);
        // the measured case: 20,594 MiB asked of a card that, that day, could not give it
        assert!(l.failed(20_594, 100));
        assert_eq!(l.budget_mib(100), Some(18_020), "an eighth under the failure");
        // the load held to that worked: it is remembered, and the budget never goes under it
        assert!(l.worked(18_195));
        assert_eq!((l.ok_mib, l.failed_mib, l.failed_at), (18_195, 20_594, 100));
        assert_eq!(l.budget_mib(200), Some(18_195));
        // a failure at or below what has worked says nothing about the card (something else held its memory)
        assert!(!l.failed(18_000, 300));
        assert_eq!((l.failed_mib, l.failed_at), (20_594, 100));
        // a smaller failure above it tightens the limit; the budget stays at what has worked, under the failure
        assert!(l.failed(18_200, 400));
        assert_eq!((l.failed_mib, l.budget_mib(400)), (18_200, Some(18_195)));
        // a failure is believed for a day, then the driver's figure stands again; what worked is kept
        assert!(l.budget_mib(400 + LIMIT_TTL_S).is_some());
        assert_eq!(l.budget_mib(400 + LIMIT_TTL_S + 1), None);
        assert!(!l.failure_is_live(400 + LIMIT_TTL_S + 1));
        assert_eq!(l.ok_mib, 18_195);
        // a failure after that starts over instead of tightening an old one
        assert!(l.failed(19_000, 400 + LIMIT_TTL_S + 10));
        assert_eq!(l.failed_mib, 19_000);
        // a load that holds what once failed lifts the limit
        assert!(l.worked(19_500));
        assert_eq!((l.failed_mib, l.budget_mib(400 + LIMIT_TTL_S + 10)), (0, None));
        // a worse run does not lower what is known to work
        assert!(!l.worked(10_000));
        assert_eq!(l.ok_mib, 19_500);
    }

    #[test]
    fn models_never_take_the_reserve_the_system_keeps() {
        let card = |total, free| Device {
            name: "Vulkan0: test".into(),
            total_mib: total,
            free_mib: free,
        };
        // the 24 GB card: 2 GB kept, so models hold at most 22,512 MiB; the margin is slack on top, not part of the cap
        let one = [card(24_560, 24_000)];
        assert_eq!(plannable_free_mib(&one, 2_048, 0, 768), 23_280);
        // a model that needs exactly the cap just fits: need + margin <= the plannable figure
        assert_eq!(plan(22_512, 23_280, 768, &[]), Plan::Fits);
        assert!(matches!(plan(22_513, 23_280, 768, &[]), Plan::Refuse { .. }));
        // free memory below the cap still rules: the desktop holds what it holds
        assert_eq!(plannable_free_mib(&[card(24_560, 20_000)], 2_048, 0, 768), 20_000);
        // what the hub's other models hold is not available again
        assert_eq!(plannable_free_mib(&one, 2_048, 10_000, 768), 13_280);
        // never underflows
        assert_eq!(plannable_free_mib(&[card(1_000, 900)], 2_048, 5_000, 0), 0);
        // no reserve: the free memory as it was
        assert_eq!(plannable_free_mib(&one, 0, 0, 768), 24_000);
    }

    #[test]
    fn the_fit_target_holds_a_load_to_its_budget() {
        // 23,392 MiB free, held to 18,021: llama-server keeps 5,371 free
        assert_eq!(fit_target_for(23_392, 18_021, 768), 5_371);
        // never less than the margin
        assert_eq!(fit_target_for(23_392, 23_000, 768), 768);
        assert_eq!(fit_target_for(1_000, 5_000, 768), 768);
        // what a load asks of a card: its need, or what the target leaves
        assert_eq!(asked_of_card(20_594, 23_392, 768), 20_594);
        assert_eq!(asked_of_card(20_594, 23_392, 5_371), 18_021);
        assert_eq!(asked_of_card(500, 100, 768), 0);
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

    fn auto(fit: Option<u64>, layers: i32) -> Resolved {
        let mut r = Resolved::new(
            (None, None, None, &ServeFlags::default()),
            None,
            &ServeProfile::default(),
            (8192, layers, 2),
        );
        r.fit_target_mib = fit;
        r
    }

    #[test]
    fn automatic_layers_are_chosen_by_llama_server_when_it_can() {
        // 999 (all) and a negative number both mean "as many as fit"
        for n in [999, 1000, -1] {
            assert!(auto(None, n).is_auto(), "{n}");
        }
        for n in [0, 1, 40, 998] {
            assert!(!auto(None, n).is_auto(), "{n}");
        }
        // told to fit: --fit on with the room to keep free, and no -ngl
        assert_eq!(
            auto(Some(768), 999).args().join(" "),
            "-c 8192 --fit on --fit-target 768 -np 2"
        );
        // not told (a server that cannot): the layers are passed as they were
        assert_eq!(auto(None, 999).args().join(" "), "-c 8192 -ngl 999 -np 2");
        // a number of layers somebody chose is never handed to --fit, whatever the target says
        assert_eq!(auto(Some(768), 20).args().join(" "), "-c 8192 -ngl 20 -np 2");
        assert_eq!(auto(Some(768), 0).args().join(" "), "-c 8192 -ngl 0 -np 2");
    }

    fn est(weights: u64, kv: u64, compute: u64) -> VramEstimate {
        VramEstimate {
            weights_mib: weights,
            kv_mib: kv,
            state_mib: 0,
            compute_mib: compute,
            total_mib: weights + kv + compute + CONTEXT_OVERHEAD_MIB as u64,
        }
    }

    #[test]
    fn a_load_goes_to_the_gpu_a_split_the_cpu_or_nowhere() {
        let e = est(8000, 1000, 300); // 9500 on the GPU
                                      // everything fits as it is
        assert_eq!(super::place(&e, 20_000, 512, &[], 32_000), Fit::Gpu);
        // after idle models leave
        let held = [res("a", 100, 6000, false), res("b", 900, 9000, false)];
        assert_eq!(
            super::place(&e, 4000, 512, &held, 32_000),
            Fit::GpuAfter(vec!["b".into()], e.total_mib + 512)
        );
        // not even then: part on the GPU, the rest from RAM (a busy model stays where it is)
        let busy = [res("a", 100, 20_000, true)];
        match super::place(&e, 6000, 512, &busy, 32_000) {
            Fit::Split { gpu_share } => assert!(gpu_share > 0.3 && gpu_share < 0.7, "{gpu_share}"),
            other => panic!("{other:?}"),
        }
        // next to no VRAM: the CPU
        assert_eq!(super::place(&e, 700, 512, &busy, 32_000), Fit::Cpu);
        // RAM cannot take the rest: refused, with the numbers
        assert_eq!(
            super::place(&e, 700, 512, &busy, 4000),
            Fit::No {
                need_ram_mib: 9768, // all 9000 of weights and KV, and 768 for the server itself
                free_ram_mib: 4000
            }
        );
    }

    #[test]
    fn paged_slots_follow_what_the_card_has_left() {
        let sh = ModelShape {
            file_bytes: 22_000 * 1_048_576,
            layers: 40,
            attn_layers: 10,
            n_embd: 2048,
            kv_full: KvElems { k: 5120, v: 5120 },
            kv_swa: KvElems::default(),
            swa_window: 0,
            state_bytes_per_slot: 0,
            expert_share: 0.8,
        };
        // 40 layers of 256 experts: a slot is 17,600 / 10,240 MiB
        let base = est(4_400, 600, 500);
        let n = paged_slots(&sh, 256, &base, 22_000).unwrap();
        assert!((7_000..9_000).contains(&n), "{n}");
        // a small card leaves too little: no paging
        assert_eq!(paged_slots(&sh, 256, &base, 5_800), None);
        // never more slots than experts
        assert_eq!(paged_slots(&sh, 2, &base, 90_000), Some(80));
        // a dense model has none
        let dense = ModelShape {
            expert_share: 0.0,
            ..sh
        };
        assert_eq!(paged_slots(&dense, 0, &base, 22_000), None);
    }

    #[test]
    fn the_cards_free_memory_takes_what_the_ram_cannot() {
        // 40 layers, a 40,000 MiB file (1,000 per layer), 32 GB of cache and state besides the weights
        let e = est(40_000, 2_000, 500);
        // fits in RAM: nothing moves
        assert_eq!(spill_layers(&e, 40, 40_000, 50_000, 24_000, 512), 0);
        // 10,000 MiB short (plus the server's own 768): 11 layers, the card has room for far more
        assert_eq!(spill_layers(&e, 40, 40_000, 30_768, 24_000, 512), 10);
        // the card holds fewer than are wanted: it takes what fits (20 of 1,050 MiB), the rest stays on disk
        assert_eq!(spill_layers(&e, 40, 40_000, 10_000, 23_000, 512), 20);
        // no room for one layer, or no layers known: stays on the CPU
        assert_eq!(spill_layers(&e, 40, 40_000, 10_000, 900, 512), 0);
        assert_eq!(spill_layers(&e, 0, 40_000, 10_000, 24_000, 512), 0);
        // never more layers than the model has
        assert_eq!(spill_layers(&e, 40, 40_000, 0, 200_000, 512), 40);
    }

    #[test]
    fn a_model_bigger_than_the_memory_is_paged_only_when_allowed() {
        let e = est(8000, 1000, 300);
        let busy = [res("a", 100, 20_000, true)];
        // not allowed: refused, exactly as place() says
        assert_eq!(
            super::place_with(&e, 700, 512, &busy, 4000, false),
            super::place(&e, 700, 512, &busy, 4000)
        );
        // allowed: the weights beyond what RAM holds are paged
        assert_eq!(
            super::place_with(&e, 700, 512, &busy, 4000, true),
            Fit::Paged {
                paged_mib: 9768 - 4000,
                need_ram_mib: 9768
            }
        );
        // the cache, the state and the server's buffers cannot be paged: too little RAM for them is still a refusal
        assert!(matches!(
            super::place_with(&e, 700, 512, &busy, 1000, true),
            Fit::No { .. }
        ));
        // a model that fits is placed as before, whatever is allowed
        assert_eq!(super::place_with(&e, 20_000, 512, &[], 4000, true), Fit::Gpu);
        let j = fit_json(&Fit::Paged {
            paged_mib: 5768,
            need_ram_mib: 9768,
        });
        assert_eq!(j["where"], "paged");
        assert_eq!(j["paged_mib"], 5768);
    }

    #[test]
    fn a_resident_model_that_runs_on_the_cpu_is_not_unloaded_for_vram() {
        let loaded = [res("cpu-model", 9999, 0, false), res("gpu-model", 10, 8000, false)];
        assert_eq!(plan(8000, 1000, 512, &loaded), Plan::Evict(vec!["gpu-model".into()]));
    }

    #[test]
    fn where_the_weights_went_is_told_from_the_memory_the_server_holds() {
        // nothing asked of the GPU
        assert_eq!(placement_of(Some(5000.0), true, Some(9000), 0, Some(32)), "cpu");
        // a server that holds no VRAM: on the CPU when the counters work, unknown when they do not
        assert_eq!(placement_of(None, true, Some(9000), 999, Some(32)), "cpu");
        assert_eq!(placement_of(None, false, Some(9000), 999, Some(32)), "unknown");
        // a GPU context alone is not the weights
        assert_eq!(placement_of(Some(90.0), true, Some(9000), 999, Some(32)), "cpu");
        // automatic layers: about all of the estimate is on the GPU, or part of it
        assert_eq!(placement_of(Some(9100.0), true, Some(9000), 999, Some(32)), "gpu");
        assert_eq!(placement_of(Some(4000.0), true, Some(9000), 999, Some(32)), "partial");
        assert_eq!(placement_of(Some(4000.0), true, None, 999, None), "gpu");
        // a number of layers: fewer than the model has is a partial offload
        assert_eq!(placement_of(Some(4000.0), true, Some(9000), 20, Some(32)), "partial");
        assert_eq!(placement_of(Some(4000.0), true, Some(9000), 40, Some(32)), "gpu");
    }

    #[test]
    fn a_placement_is_shown_to_the_api_by_where_it_goes() {
        assert_eq!(fit_json(&Fit::Gpu)["where"], "gpu");
        assert_eq!(fit_json(&Fit::Cpu)["where"], "cpu");
        let split = fit_json(&Fit::Split { gpu_share: 0.4567 });
        assert_eq!(
            (split["where"].as_str(), split["gpu_share"].as_f64()),
            (Some("split"), Some(0.46))
        );
        let after = fit_json(&Fit::GpuAfter(vec!["x".into()], 9000));
        assert_eq!(
            (after["where"].as_str(), after["unload"][0].as_str()),
            (Some("gpu_after_unloading"), Some("x"))
        );
        let no = fit_json(&Fit::No {
            need_ram_mib: 10,
            free_ram_mib: 5,
        });
        assert_eq!(
            (no["where"].as_str(), no["need_ram_mib"].as_u64()),
            (Some("none"), Some(10))
        );
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
    fn layer_readahead_is_a_rigel_option_and_zero_means_off() {
        let on = ServeFlags {
            engine: Some("rigel".into()),
            layer_readahead: Some(4),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &on), None, &ServeProfile::default(), (0, 999, 1));
        r.validate().unwrap();
        assert!(r.args().join(" ").ends_with("--layer-readahead 4"), "{:?}", r.args());
        let off = ServeFlags {
            layer_readahead: Some(0),
            ..on.clone()
        };
        let r = Resolved::new((None, None, None, &off), None, &ServeProfile::default(), (0, 999, 1));
        assert!(!r.args().join(" ").contains("readahead"));
        // llama.cpp does not have it
        let plain = ServeFlags {
            layer_readahead: Some(4),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &plain), None, &ServeProfile::default(), (0, 999, 1));
        assert!(r.validate().unwrap_err().contains("layer_readahead is a Rigel option"));
    }

    #[test]
    fn layer_resident_mib_is_a_rigel_option_where_zero_is_off_and_minus_one_is_what_is_free() {
        let on = ServeFlags {
            engine: Some("rigel".into()),
            layer_readahead: Some(4),
            layer_resident_mib: Some(2048),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &on), None, &ServeProfile::default(), (0, 0, 1));
        r.validate().unwrap();
        assert!(
            r.args()
                .join(" ")
                .ends_with("--layer-readahead 4 --layer-resident-mib 2048"),
            "{:?}",
            r.args()
        );
        let auto = ServeFlags {
            layer_resident_mib: Some(-1),
            ..on.clone()
        };
        let r = Resolved::new((None, None, None, &auto), None, &ServeProfile::default(), (0, 0, 1));
        r.validate().unwrap();
        assert!(
            r.args().join(" ").ends_with("--layer-resident-mib -1"),
            "{:?}",
            r.args()
        );
        let off = ServeFlags {
            layer_resident_mib: Some(0),
            ..on.clone()
        };
        let r = Resolved::new((None, None, None, &off), None, &ServeProfile::default(), (0, 0, 1));
        assert!(!r.args().join(" ").contains("resident"));
        // below -1 means nothing
        let bad = ServeFlags {
            layer_resident_mib: Some(-5),
            ..on.clone()
        };
        assert!(validate_flags(&bad, Some(1))
            .unwrap_err()
            .contains("layer_resident_mib"));
        // llama.cpp does not have it
        let plain = ServeFlags {
            layer_resident_mib: Some(2048),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &plain), None, &ServeProfile::default(), (0, 0, 1));
        assert!(r
            .validate()
            .unwrap_err()
            .contains("layer_resident_mib is a Rigel option"));
    }

    #[test]
    fn rigel_gets_one_slot_its_flags_and_its_cache_variable() {
        let rigel = ServeFlags {
            engine: Some("rigel".into()),
            moe_direct_io: Some(true),
            moe_slots: Some(5000),
            moe_pack_dir: Some(PathBuf::from("bundle")),
            moe_profile: Some(PathBuf::from("bundle/profile.bin")),
            moe_cache_mib: Some(4096),
            layer_readahead: Some(4),
            ..Default::default()
        };
        // the engine in [serving], the options in the model's profile: they still meet
        let all = ServeProfile {
            flags: ServeFlags {
                engine: Some("rigel".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let model = ServeProfile {
            parallel: Some(4),
            ctx_size: Some(8192),
            flags: ServeFlags {
                engine: None,
                ..rigel.clone()
            },
            ..Default::default()
        };
        let r = Resolved::new(
            (None, None, None, &ServeFlags::default()),
            Some(&model),
            &all,
            (0, 999, 2),
        );
        assert_eq!(r.slots, 1, "Rigel serves one request at a time");
        r.validate().unwrap();
        assert_eq!(
            r.args().join(" "),
            "-c 8192 -ngl 999 -np 1 --moe-direct-io --moe-slots 5000 --moe-pack-dir bundle --moe-profile bundle/profile.bin --layer-readahead 4"
        );
        assert_eq!(r.env(), [("MOE_CACHE_MIB".to_string(), "4096".to_string())]);
        assert_eq!(Serving::from_resolved(&r).engine, "rigel");

        // llama.cpp: no Rigel flag, no variable, and a Rigel option is an error
        let plain = Resolved::new(
            (None, None, Some(3), &ServeFlags::default()),
            None,
            &ServeProfile::default(),
            (0, 999, 2),
        );
        assert_eq!(plain.slots, 3);
        assert!(plain.env().is_empty());
        assert_eq!(Serving::from_resolved(&plain).engine, "llama.cpp");
        let stray = ServeFlags {
            moe_slots: Some(10),
            ..Default::default()
        };
        let r = Resolved::new((None, None, None, &stray), None, &ServeProfile::default(), (0, 999, 1));
        let e = r.validate().unwrap_err();
        assert!(e.contains("moe_slots is a Rigel option"), "{e}");
        assert!(!r.args().iter().any(|a| a.starts_with("--moe")));
    }

    #[test]
    fn an_unknown_engine_is_refused_and_dropped_with_a_warning() {
        let f = ServeFlags {
            engine: Some("vllm".into()),
            ..Default::default()
        };
        assert!(validate_flags(&f, None).unwrap_err().contains("engine \"vllm\""));
        let zero = ServeFlags {
            engine: Some("rigel".into()),
            moe_slots: Some(0),
            ..Default::default()
        };
        assert!(validate_flags(&zero, None).unwrap_err().contains("moe_slots"));
        let table: toml::Table = toml::from_str("engine = \"vllm\"\nmoe_slots = 0\nctx_size = 4096").unwrap();
        let (p, w) = ServeProfile::lenient(&table, "[profiles.x]");
        assert_eq!(p.flags.engine, None);
        assert_eq!(p.flags.moe_slots, None);
        assert_eq!(p.ctx_size, Some(4096));
        assert_eq!(w.len(), 2, "{w:?}");
        let table: toml::Table = toml::from_str("engine = \"rigel\"\nmoe_direct_io = true").unwrap();
        let (p, w) = ServeProfile::lenient(&table, "[profiles.x]");
        assert!(w.is_empty(), "{w:?}");
        assert!(p.flags.is_rigel());
        assert_eq!(p.flags.moe_direct_io, Some(true));
    }

    #[test]
    fn every_flag_is_built() {
        let f = ServeFlags {
            paged: None,
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
            engine: None,
            moe_direct_io: None,
            moe_slots: None,
            moe_pack_dir: None,
            moe_profile: None,
            moe_cache_mib: None,
            layer_readahead: None,
            layer_resident_mib: None,
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

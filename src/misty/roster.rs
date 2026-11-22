//! The roster: which model fills which role, and whether it can run on this machine right now.
//!
//! Misty's code names roles, never models. A role resolves, in order, to the id the owner put in `config.toml`
//! (`[roles]`), then to the first installed model that matches the role's preferred names, then (for the three
//! language-model roles) to an installed chat model of a suitable size. A file the available engines cannot run
//! correctly (a ternary Bonsai file on a llama.cpp that predates its kernels) is never chosen: it would load and
//! answer wrongly without any warning.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::catalog::ModelEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Reflex,
    Fast,
    Main,
    Deep,
    Ear,
    Eye,
    Embed,
    Rerank,
}

pub const ALL: [Role; 8] = [
    Role::Reflex,
    Role::Fast,
    Role::Main,
    Role::Deep,
    Role::Ear,
    Role::Eye,
    Role::Embed,
    Role::Rerank,
];

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Reflex => "reflex",
            Role::Fast => "fast",
            Role::Main => "main",
            Role::Deep => "deep",
            Role::Ear => "ear",
            Role::Eye => "eye",
            Role::Embed => "embed",
            Role::Rerank => "rerank",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        ALL.iter().copied().find(|r| r.name() == s.trim().to_lowercase())
    }

    /// The language-model roles, cheapest first.
    pub fn is_brain(self) -> bool {
        matches!(self, Role::Fast | Role::Main | Role::Deep)
    }

    /// Roles served by the Cactus runtime (not a GGUF model in the catalog).
    pub fn is_cactus(self) -> bool {
        matches!(self, Role::Reflex | Role::Ear | Role::Eye)
    }

    /// The next brain up, for escalation.
    pub fn up(self) -> Option<Role> {
        match self {
            Role::Fast => Some(Role::Main),
            Role::Main => Some(Role::Deep),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Role::Reflex => "chooses tools and fills their arguments (Needle 3)",
            Role::Fast => "short, simple turns",
            Role::Main => "the default brain",
            Role::Deep => "hard reasoning and long synthesis",
            Role::Ear => "speech to text (Whistle)",
            Role::Eye => "image understanding (Mirage)",
            Role::Embed => "dense retrieval vectors",
            Role::Rerank => "second-stage ranking",
        }
    }

    /// Lowercase name fragments of the models preferred for the role, best first.
    fn preferred(self) -> &'static [&'static str] {
        match self {
            Role::Fast => &["bonsai-2-2b", "bonsai-2-4b", "qwen3.5-2b"],
            Role::Main => &["bonsai-2-9b", "bonsai-2-4b", "qwen3.5-9b"],
            Role::Deep => &["bonsai-2-27b", "ternary-bonsai-27b", "qwen3.5-27b"],
            Role::Embed => &["embed", "bge", "e5-", "gte-", "nomic"],
            Role::Rerank => &["rerank"],
            _ => &[],
        }
    }
}

/// What the machine's engines can run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Engines {
    /// A llama-server new enough for `Q2_0` (llama.cpp b11556 or later).
    pub stock_q2: bool,
    /// The Prism llama-server is installed: runs every ternary packing.
    pub prism: bool,
}

/// The ternary packing of a file, from its id or path: `PTQ1_0`, `PQ2_0` or `Q2_0`.
pub fn ternary_pack(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    // the longer names first: "ptq1_0" and "pq2_0" both contain "q"-digits patterns
    if n.contains("ptq1_0") {
        Some("PTQ1_0")
    } else if n.contains("pq2_0") {
        Some("PQ2_0")
    } else if n.contains("q2_0") && !n.contains("iq2") {
        Some("Q2_0")
    } else {
        None
    }
}

/// The ternary packing of a catalog entry: from its id, else from its file name.
pub fn pack_of(e: &ModelEntry) -> Option<&'static str> {
    ternary_pack(&e.id).or_else(|| e.path.file_name().and_then(|n| n.to_str()).and_then(ternary_pack))
}

/// Can these engines run this file correctly?
pub fn runnable(name: &str, e: Engines) -> bool {
    match ternary_pack(name) {
        None => true,
        Some("Q2_0") => e.stock_q2 || e.prism,
        Some(_) => e.prism,
    }
}

/// Is the entry a language model that can chat (not an embedder, a reranker, a projector or a speech model)?
pub fn is_chat(e: &ModelEntry) -> bool {
    let id = e.id.to_lowercase();
    let arch = e.architecture().unwrap_or("");
    !(id.contains("embed")
        || id.contains("rerank")
        || id.contains("mmproj")
        || id.contains("dflash")
        || id.contains("whisper")
        || id.contains("bge-")
        || id.contains("e5-")
        || arch.contains("bert")
        || arch == "clip"
        || arch == "dflash")
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Resolution {
    pub role: Role,
    pub model: Option<String>,
    /// `ready`, `missing`, `unfit` (does not fit free RAM) or `unavailable` (needs an engine that is not installed).
    pub status: &'static str,
    /// `config`, `preferred`, `fallback` or `cactus`.
    pub source: &'static str,
    pub size_bytes: u64,
    pub note: String,
}

/// Prefer the packing the machine runs fastest: PQ2_0 (the Bonsai demo's default for speed), then Q2_0, then PTQ1_0.
fn pack_rank(name: &str) -> u8 {
    match ternary_pack(name) {
        Some("PQ2_0") => 0,
        Some("Q2_0") => 1,
        Some("PTQ1_0") => 2,
        _ => 3,
    }
}

fn fits(size: u64, free_ram: u64, margin: u64) -> bool {
    // the weights plus roughly a tenth for the KV cache and buffers
    size.saturating_add(size / 10).saturating_add(margin) <= free_ram
}

/// Resolve every GGUF role. `overrides` is `[roles]` from the config; `free_ram` in bytes; `margin` in bytes.
pub fn resolve(
    catalog: &[ModelEntry],
    overrides: &BTreeMap<String, String>,
    engines: Engines,
    free_ram: u64,
    margin: u64,
) -> Vec<Resolution> {
    let mut out = vec![];
    for role in [Role::Fast, Role::Main, Role::Deep, Role::Embed, Role::Rerank] {
        out.push(resolve_one(catalog, overrides, engines, free_ram, margin, role));
    }
    out
}

fn resolve_one(
    catalog: &[ModelEntry],
    overrides: &BTreeMap<String, String>,
    engines: Engines,
    free_ram: u64,
    margin: u64,
    role: Role,
) -> Resolution {
    let mk = |e: Option<&ModelEntry>, status, source, note: String| Resolution {
        role,
        model: e.map(|e| e.id.clone()),
        status,
        source,
        size_bytes: e.map(|e| e.size_bytes).unwrap_or(0),
        note,
    };
    // 1. the owner's choice
    if let Some(want) = overrides.get(role.name()) {
        return match catalog
            .iter()
            .find(|e| e.id == *want || e.id.eq_ignore_ascii_case(want))
        {
            None => mk(
                None,
                "missing",
                "config",
                format!("{want} is named in [roles] but is not installed"),
            ),
            Some(e) if !runnable(&e.id, engines) => mk(
                Some(e),
                "unavailable",
                "config",
                format!("{} needs the Prism engine (run `model-mistress misty setup`)", e.id),
            ),
            Some(e) if role.is_brain() && !fits(e.size_bytes, free_ram, margin) => {
                mk(Some(e), "unfit", "config", "does not fit the free RAM".into())
            }
            Some(e) => mk(Some(e), "ready", "config", String::new()),
        };
    }
    let usable = |e: &&ModelEntry| {
        if role.is_brain() {
            is_chat(e)
        } else if role == Role::Embed {
            let id = e.id.to_lowercase();
            id.contains("embed")
                || id.contains("bge")
                || id.contains("e5-")
                || id.contains("gte-")
                || id.contains("nomic")
        } else {
            e.id.to_lowercase().contains("rerank")
        }
    };
    // 2. a preferred name, among files these engines can run; best packing first
    let mut seen_unrunnable: Option<&ModelEntry> = None;
    for frag in role.preferred() {
        let mut hits: Vec<&ModelEntry> = catalog
            .iter()
            .filter(usable)
            .filter(|e| e.id.to_lowercase().contains(frag))
            .collect();
        hits.sort_by_key(|e| (pack_rank(&e.id), e.size_bytes));
        for e in hits {
            if !runnable(&e.id, engines) {
                seen_unrunnable.get_or_insert(e);
                continue;
            }
            if role.is_brain() && !fits(e.size_bytes, free_ram, margin) {
                return mk(Some(e), "unfit", "preferred", "does not fit the free RAM".into());
            }
            return mk(Some(e), "ready", "preferred", String::new());
        }
    }
    if let Some(e) = seen_unrunnable {
        return mk(
            Some(e),
            "unavailable",
            "preferred",
            format!("{} needs the Prism engine (run `model-mistress misty setup`)", e.id),
        );
    }
    // 3. an installed chat model of a suitable size
    if role.is_brain() {
        let gb = |n: f64| (n * 1e9) as u64;
        let (lo, hi) = match role {
            Role::Fast => (0, gb(3.5)),
            Role::Main => (gb(3.5), gb(14.0)),
            _ => (gb(14.0), u64::MAX),
        };
        let mut hits: Vec<&ModelEntry> = catalog
            .iter()
            .filter(usable)
            .filter(|e| e.size_bytes > lo && e.size_bytes <= hi && runnable(&e.id, engines))
            .collect();
        // the largest that fits: a bigger model of the class is the better one
        hits.sort_by_key(|e| std::cmp::Reverse(e.size_bytes));
        if let Some(e) = hits.into_iter().find(|e| fits(e.size_bytes, free_ram, margin)) {
            return mk(
                Some(e),
                "ready",
                "fallback",
                "an installed model of this size class; setup can add a better one".into(),
            );
        }
    }
    mk(
        None,
        "missing",
        "preferred",
        format!("no {} model installed (run `model-mistress misty setup`)", role.name()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(id: &str, gb: f64, arch: &str) -> ModelEntry {
        let mut meta = serde_json::Map::new();
        meta.insert("general.architecture".into(), arch.into());
        ModelEntry {
            id: id.into(),
            source: "folder",
            path: PathBuf::from(format!("{id}.gguf")),
            size_bytes: (gb * 1e9) as u64,
            mmproj: None,
            meta,
            native: None,
        }
    }

    const GB: u64 = 1_000_000_000;

    #[test]
    fn packings_are_recognised_and_ranked() {
        assert_eq!(ternary_pack("Ternary-Bonsai-2-9B-PTQ1_0"), Some("PTQ1_0"));
        assert_eq!(ternary_pack("Ternary-Bonsai-2-9B-PQ2_0"), Some("PQ2_0"));
        assert_eq!(ternary_pack("Ternary-Bonsai-2-9B-Q2_0"), Some("Q2_0"));
        assert_eq!(ternary_pack("some-IQ2_XS"), None);
        assert_eq!(ternary_pack("qwen3-8b-Q4_K_M"), None);
    }

    #[test]
    fn a_ternary_file_is_refused_where_its_kernels_are_missing() {
        let none = Engines::default();
        assert!(!runnable("x-PQ2_0", none) && !runnable("x-PTQ1_0", none) && !runnable("x-Q2_0", none));
        assert!(runnable(
            "x-Q2_0",
            Engines {
                stock_q2: true,
                prism: false
            }
        ));
        assert!(!runnable(
            "x-PQ2_0",
            Engines {
                stock_q2: true,
                prism: false
            }
        ));
        assert!(runnable(
            "x-PQ2_0",
            Engines {
                stock_q2: false,
                prism: true
            }
        ));
        assert!(runnable("qwen3-8b-Q4_K_M", none));
    }

    #[test]
    fn preferred_models_win_and_the_best_packing_is_chosen() {
        let cat = vec![
            entry("Ternary-Bonsai-2-9B-PTQ1_0", 2.0, "qwen35"),
            entry("Ternary-Bonsai-2-9B-PQ2_0", 2.42, "qwen35"),
            entry("Ternary-Bonsai-2-2B-PQ2_0", 0.52, "qwen35"),
            entry("nomic-embed-text", 0.3, "nomic-bert"),
        ];
        let r = resolve(
            &cat,
            &BTreeMap::new(),
            Engines {
                stock_q2: false,
                prism: true,
            },
            30 * GB,
            2 * GB,
        );
        let by = |role: Role| r.iter().find(|x| x.role == role).unwrap();
        assert_eq!(by(Role::Main).model.as_deref(), Some("Ternary-Bonsai-2-9B-PQ2_0"));
        assert_eq!(by(Role::Fast).model.as_deref(), Some("Ternary-Bonsai-2-2B-PQ2_0"));
        assert_eq!(by(Role::Embed).model.as_deref(), Some("nomic-embed-text"));
        assert_eq!(by(Role::Deep).status, "missing");
        assert_eq!(by(Role::Rerank).status, "missing");
    }

    #[test]
    fn without_the_prism_engine_the_role_says_so_instead_of_loading_garbage() {
        let cat = vec![entry("Ternary-Bonsai-2-9B-PQ2_0", 2.42, "qwen35")];
        let r = resolve(&cat, &BTreeMap::new(), Engines::default(), 30 * GB, 2 * GB);
        let main = r.iter().find(|x| x.role == Role::Main).unwrap();
        assert_eq!(main.status, "unavailable");
        assert!(main.note.contains("misty setup"));
    }

    #[test]
    fn a_model_that_does_not_fit_free_ram_is_unfit_and_a_fallback_is_by_size_class() {
        let cat = vec![
            entry("Ternary-Bonsai-2-27B-PQ2_0", 7.21, "qwen35"),
            entry("llama3.2-1b-Q4_K_M", 0.8, "llama"),
            entry("qwen3-8b-Q4_K_M", 5.0, "qwen3"),
        ];
        let eng = Engines {
            stock_q2: false,
            prism: true,
        };
        let r = resolve(&cat, &BTreeMap::new(), eng, 6 * GB, 2 * GB);
        let by = |role: Role| r.iter().find(|x| x.role == role).unwrap();
        assert_eq!(by(Role::Deep).status, "unfit");
        assert_eq!(by(Role::Fast).model.as_deref(), Some("llama3.2-1b-Q4_K_M"));
        assert_eq!(by(Role::Fast).source, "fallback");
        // 5 GB + a tenth + a 2 GB margin does not fit 6 GB: no main model rather than one that swaps
        assert_eq!(by(Role::Main).status, "missing");
    }

    #[test]
    fn the_owners_choice_beats_the_preferences() {
        let cat = vec![
            entry("Ternary-Bonsai-2-9B-PQ2_0", 2.42, "qwen35"),
            entry("mine", 1.0, "llama"),
        ];
        let mut o = BTreeMap::new();
        o.insert("main".to_string(), "mine".to_string());
        o.insert("deep".to_string(), "gone".to_string());
        let r = resolve(
            &cat,
            &o,
            Engines {
                stock_q2: false,
                prism: true,
            },
            30 * GB,
            2 * GB,
        );
        let by = |role: Role| r.iter().find(|x| x.role == role).unwrap();
        assert_eq!(
            (by(Role::Main).model.as_deref(), by(Role::Main).source),
            (Some("mine"), "config")
        );
        assert_eq!(by(Role::Deep).status, "missing");
    }

    #[test]
    fn roles_parse_and_escalate_upward_only() {
        assert_eq!(Role::parse(" Main "), Some(Role::Main));
        assert_eq!(Role::parse("x"), None);
        assert_eq!(Role::Fast.up(), Some(Role::Main));
        assert_eq!(Role::Main.up(), Some(Role::Deep));
        assert_eq!(Role::Deep.up(), None);
        assert!(Role::Reflex.is_cactus() && !Role::Main.is_cactus());
    }
}

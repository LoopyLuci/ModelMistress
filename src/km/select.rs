//! Finding a KM (km-spec.md §6): filters first, then a score whose every term is recorded.
//!
//! `score = w_vec·cos(query, centroid) + w_lex·bm25 + w_route·router_logit + prior + pin_bonus`
//!
//! The vector term needs the query's embedding and a KM whose centroid was made by the same embedder; otherwise it is
//! dropped for that KM and the trace says why. The router term is 0 until a `router` KM drives it (not yet).
//! A query may carry exact conditions: `tag:lang:rust domain:code/* kind:adapter`.

use std::collections::HashMap;

use semver::{Version, VersionReq};
use serde_json::{json, Value};

use super::embed::{cosine, f16_to_f32};
use super::residency::declared;
use super::store::{Owner, Record, Store};
use super::Res;

#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Free text plus any `tag:`, `domain:` and `kind:` conditions.
    pub text: String,
    pub kinds: Vec<String>,
    pub tags: Vec<String>,
    pub domains: Vec<String>,
    /// The core the KMs would be used with: `dusty`, `misty`.
    pub base: Option<String>,
    pub base_version: Option<Version>,
    pub arch: Option<String>,
    /// Modalities the request brings / needs back; a KM must handle them all.
    pub input: Vec<String>,
    pub output: Vec<String>,
    /// Channel to read each KM's version from (default `stable`, else the newest).
    pub channel: Option<String>,
    pub all_versions: bool,
    pub top_k: usize,
    /// Also KMs the core has absorbed into its weights (skipped by default).
    pub include_absorbed: bool,
    /// A tag counts only if its confidence is at least this (0 = any).
    pub min_tag_confidence: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub text: String,
    pub tags: Vec<String>,
    pub domains: Vec<String>,
    pub kinds: Vec<String>,
}

/// Splits a query into its exact conditions and its free text.
pub fn parse_structured(q: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut free = vec![];
    for tok in q.split_whitespace() {
        if let Some(v) = tok.strip_prefix("tag:").filter(|v| !v.is_empty()) {
            p.tags.push(v.to_string());
        } else if let Some(v) = tok.strip_prefix("domain:").filter(|v| !v.is_empty()) {
            p.domains.push(v.to_string());
        } else if let Some(v) = tok.strip_prefix("kind:").filter(|v| !v.is_empty()) {
            p.kinds.push(v.to_string());
        } else {
            free.push(tok);
        }
    }
    p.text = free.join(" ");
    p
}

/// `code/*` is everything under `code/`; `*` is everything; anything else must match exactly.
pub fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix("/*") {
        Some(prefix) => domain.starts_with(&format!("{prefix}/")),
        None => pattern == domain,
    }
}

#[derive(Debug, Clone)]
pub struct Weights {
    pub vec: f64,
    pub lex: f64,
    pub route: f64,
    pub pin_bonus: f64,
    /// Weight of the confidence of the tags the query asked for.
    pub tag: f64,
}

/// The largest a single KM may be for the core asked about (0 = no limit).
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    pub ram: u64,
    pub vram: u64,
}

/// The query's embedding and whose it is.
pub struct QueryVec {
    pub embedder: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub record: Record,
    pub score: f64,
    pub terms: Value,
    pub notes: Vec<String>,
    pub via: String,
}

#[derive(Debug, Clone)]
pub struct Excluded {
    pub key: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub ranked: Vec<Scored>,
    pub excluded: Vec<Excluded>,
    pub notes: Vec<String>,
    pub weights: Weights,
    pub embedder: Option<String>,
}

impl Selection {
    pub fn to_json(&self) -> Value {
        json!({
            "weights": {"vec": self.weights.vec, "lex": self.weights.lex, "route": self.weights.route,
                        "pin_bonus": self.weights.pin_bonus, "tag": self.weights.tag},
            "embedder": self.embedder,
            "notes": self.notes,
            "results": self.ranked.iter().enumerate().map(|(i, s)| json!({
                "rank": i + 1, "key": s.record.key, "id": s.record.id, "version": s.record.version,
                "kind": s.record.kind, "name": s.record.name, "domain": s.record.domain, "score": s.score,
                "terms": s.terms, "notes": s.notes, "version_chosen_by": s.via})).collect::<Vec<_>>(),
            "excluded": self.excluded.iter().map(|e| json!({"key": e.key, "reasons": e.reasons})).collect::<Vec<_>>(),
        })
    }
}

fn pick_versions(store: &Store, all: Vec<Record>, q: &Query) -> Vec<(Record, String)> {
    if q.all_versions {
        return all.into_iter().map(|r| (r, "all versions".into())).collect();
    }
    let channel = q.channel.clone().unwrap_or_else(|| "stable".into());
    let mut by_id: Vec<(String, Vec<Record>)> = vec![];
    for r in all {
        match by_id.last_mut() {
            Some((id, v)) if *id == r.id => v.push(r),
            _ => by_id.push((r.id.clone(), vec![r])),
        }
    }
    let mut out = vec![];
    for (id, versions) in by_id {
        let on_channel = store
            .channel_get(&id, &channel)
            .and_then(|v| versions.iter().find(|r| r.version == v).cloned());
        match on_channel {
            Some(r) => out.push((r, format!("channel {channel}"))),
            None => {
                if let Some(r) = versions.into_iter().next() {
                    // `all` is newest first within an id
                    out.push((r, format!("newest (no {channel} channel)")));
                }
            }
        }
    }
    out
}

/// Filters and scores every installed KM for a query.
pub fn rank(
    store: &Store,
    q: &Query,
    w: &Weights,
    budget: Budget,
    qvec: Result<Option<QueryVec>, String>,
) -> Res<Selection> {
    let parsed = parse_structured(&q.text);
    let mut tags = q.tags.clone();
    tags.extend(parsed.tags);
    let mut domains = q.domains.clone();
    domains.extend(parsed.domains);
    let mut kinds = q.kinds.clone();
    kinds.extend(parsed.kinds);
    let text = parsed.text;

    let mut notes = vec![];
    let (qvec, embedder) = match qvec {
        Ok(Some(v)) => {
            let id = v.embedder.clone();
            (Some(v), Some(id))
        }
        Ok(None) => {
            notes.push("vector term off: no query text to embed".to_string());
            (None, None)
        }
        Err(e) => {
            notes.push(format!("vector term off for every KM: the embedder failed ({e})"));
            (None, None)
        }
    };
    notes.push("router term is 0: no router KM is active".to_string());

    let lexical = if text.trim().is_empty() {
        HashMap::new()
    } else {
        store.lexical(&text)?
    };
    let tag_keys: Vec<HashMap<String, (String, f64)>> =
        tags.iter().map(|t| store.keys_with_tag(t)).collect::<Res<_>>()?;
    let owner: HashMap<String, Owner> = store.owner_all();
    let candidates = pick_versions(store, store.all(true)?, q);

    let mut ranked = vec![];
    let mut excluded = vec![];
    for (r, via) in candidates {
        let mut why: Vec<String> = vec![];
        let o = owner.get(&r.id).cloned().unwrap_or_default();
        if o.banned {
            why.push(format!(
                "banned by the owner{}",
                o.note.map(|n| format!(" ({n})")).unwrap_or_default()
            ));
        }
        if let Some(m) = &r.misdeclared {
            why.push(format!("misdeclared: {m}"));
        }
        if !kinds.is_empty() && !kinds.contains(&r.kind) {
            why.push(format!("kind {} is not one of {}", r.kind, kinds.join(", ")));
        }
        let mut tag_conf = vec![];
        for (t, keys) in tags.iter().zip(&tag_keys) {
            match keys.get(&r.key) {
                None => why.push(format!("missing tag {t}")),
                Some((origin, c)) if *c < q.min_tag_confidence => why.push(format!(
                    "tag {t} has confidence {c:.2} ({origin}), below the {:.2} asked for",
                    q.min_tag_confidence
                )),
                Some((_, c)) => tag_conf.push(*c),
            }
        }
        // absorbed into the weights of the core asked about (or of the KM's own base): skipped unless asked for
        let core = q.base.clone().unwrap_or_else(|| r.base.clone());
        if !q.include_absorbed {
            if let Some(c) = r.consolidation.get(&core).or_else(|| r.consolidation.get("any")) {
                if c["state"] == "absorbed" {
                    why.push(format!(
                        "absorbed into {core}'s weights (uplift {}, {} steps); ask for include_absorbed to see it",
                        c["uplift"], c["steps"]
                    ));
                }
            }
        }
        for d in &domains {
            if !domain_matches(d, &r.domain) {
                why.push(format!("domain {:?} does not match {d}", r.domain));
            }
        }
        if let Some(b) = &q.base {
            if r.base != "any" && &r.base != b {
                why.push(format!("built for base {}, not {b}", r.base));
            }
        }
        if let (Some(range), Some(v)) = (&r.base_version, &q.base_version) {
            if let Ok(req) = VersionReq::parse(range) {
                if !req.matches(v) {
                    why.push(format!("needs base version {range}, the core is {v}"));
                }
            }
        }
        if let (Some(a), Some(b)) = (&r.arch, &q.arch) {
            if a != b {
                why.push(format!("built for architecture {a}, not {b}"));
            }
        }
        for m in &q.input {
            if !r.input.contains(m) {
                why.push(format!("does not take {m} input"));
            }
        }
        for m in &q.output {
            if !r.output.contains(m) {
                why.push(format!("does not produce {m} output"));
            }
        }
        let (ram, vram) = declared(&r);
        if budget.ram > 0 && ram > budget.ram {
            why.push(format!(
                "can never fit: needs {ram} bytes of RAM, the budget is {}",
                budget.ram
            ));
        }
        if budget.vram > 0 && vram > budget.vram {
            why.push(format!(
                "can never fit: needs {vram} bytes of VRAM, the budget is {}",
                budget.vram
            ));
        }
        if !why.is_empty() {
            excluded.push(Excluded {
                key: r.key,
                reasons: why,
            });
            continue;
        }

        let mut kn = vec![];
        let (vec_raw, vec_used) = match (&qvec, &r.embed_model, &r.centroid) {
            (None, _, _) => (0.0, false),
            (Some(_), None, _) | (Some(_), _, None) => {
                kn.push("vector term dropped: this KM has no centroid".to_string());
                (0.0, false)
            }
            (Some(qv), Some(em), Some(c)) if *em != qv.embedder => {
                kn.push(format!(
                    "vector term dropped: the centroid was made by {em}, the selector embeds with {}",
                    qv.embedder
                ));
                (0.0, false)
            }
            (Some(qv), Some(_), Some(c)) => {
                let cv = f16_to_f32(c);
                if cv.len() != qv.vector.len() {
                    kn.push(format!(
                        "vector term dropped: centroid has {} dimensions, the query {}",
                        cv.len(),
                        qv.vector.len()
                    ));
                    (0.0, false)
                } else {
                    (cosine(&qv.vector, &cv) as f64, true)
                }
            }
        };
        let lex_raw = lexical.get(&r.key).copied().unwrap_or(0.0);
        let prior = r.prior;
        let pin = if o.pinned { w.pin_bonus } else { 0.0 };
        let tag_raw: f64 = tag_conf.iter().sum();
        let score = w.vec * vec_raw + w.lex * lex_raw + w.route * 0.0 + prior + pin + w.tag * tag_raw;
        let terms = json!({
            "vec": {"raw": vec_raw, "weight": w.vec, "value": w.vec * vec_raw, "used": vec_used},
            "lex": {"raw": lex_raw, "weight": w.lex, "value": w.lex * lex_raw},
            "route": {"raw": 0.0, "weight": w.route, "value": 0.0},
            "prior": {"value": prior},
            "pin": {"pinned": o.pinned, "value": pin},
            "tags": {"matched": tag_conf.len(), "confidence_sum": tag_raw, "weight": w.tag, "value": w.tag * tag_raw},
        });
        ranked.push(Scored {
            record: r,
            score,
            terms,
            notes: kn,
            via,
        });
    }
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.record.key.cmp(&b.record.key))
    });
    if q.top_k > 0 {
        ranked.truncate(q.top_k);
    }
    excluded.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Selection {
        ranked,
        excluded,
        notes,
        weights: w.clone(),
        embedder,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::embed::{Embedder, HashEmbedder};
    use crate::km::manifest::Manifest;
    use crate::km::pack::{SigState, Source};
    use crate::km::sample::{self, Builder};
    use crate::km::store::Trust;
    use std::path::{Path, PathBuf};

    fn setup() -> (PathBuf, Store) {
        let home = crate::testing::temp_home();
        let s = Store::open(&home).unwrap();
        (home, s)
    }

    fn add(s: &Store, dir: &Path) {
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(dir, &m).unwrap();
        let t = Trust {
            source: Source::Own,
            sig: SigState::Unsigned,
            overridden: false,
        };
        s.install(&stage, &m, &t, 0).unwrap();
    }

    fn weights() -> Weights {
        Weights {
            vec: 1.0,
            lex: 0.25,
            route: 1.0,
            pin_bonus: 2.0,
            tag: 0.5,
        }
    }

    fn qv(text: &str) -> Result<Option<QueryVec>, String> {
        let e = HashEmbedder::default();
        Ok(Some(QueryVec {
            embedder: e.model_id(),
            vector: e.embed_one(text),
        }))
    }

    fn query(text: &str) -> Query {
        Query {
            text: text.into(),
            top_k: 10,
            ..Default::default()
        }
    }

    fn library(home: &Path, s: &Store) {
        let src = home.join("src");
        add(
            s,
            &sample::adapter(
                &src,
                "code.rust.async",
                "1.0.0",
                "async rust tokio futures select cancellation",
            ),
        );
        add(
            s,
            &sample::adapter(&src, "code.sql", "1.0.0", "sql postgres joins indexes query planner"),
        );
        add(
            s,
            &Builder::new(&src, "memory", "docs.cooking", "1.0.0")
                .descriptor("sourdough bread flour hydration baking")
                .domain("docs/cooking")
                .tags(&["lang:en"])
                .tables("[memory]\nchunks = \"payload/c.jsonl\"\nembeddings = \"payload/e.f16\"\ndim = 384")
                .file(
                    "payload/c.jsonl",
                    "chunks",
                    "jsonl",
                    b"{\"id\":\"c1\",\"text\":\"x\"}\n".to_vec(),
                )
                .file("payload/e.f16", "embeddings", "f16", vec![0; 768])
                .build(),
        );
    }

    #[test]
    fn structured_conditions_are_split_from_the_text() {
        let p = parse_structured("tag:lang:rust domain:code/* kind:adapter how to cancel a task tag:");
        assert_eq!(p.tags, vec!["lang:rust"]);
        assert_eq!(p.domains, vec!["code/*"]);
        assert_eq!(p.kinds, vec!["adapter"]);
        assert_eq!(p.text, "how to cancel a task tag:");
        assert!(domain_matches("code/*", "code/rust") && domain_matches("code/*", "code/rust/async"));
        assert!(!domain_matches("code/*", "code") && !domain_matches("code/*", "codex/rust"));
        assert!(domain_matches("code/rust", "code/rust") && !domain_matches("code/rust", "code/rust/async"));
        assert!(domain_matches("*", "anything"));
    }

    #[test]
    fn the_best_match_ranks_first_and_every_term_is_recorded() {
        let (home, s) = setup();
        library(&home, &s);
        let sel = rank(
            &s,
            &query("cancel a tokio task with select"),
            &weights(),
            Budget::default(),
            qv("cancel a tokio task with select"),
        )
        .unwrap();
        assert_eq!(sel.ranked[0].record.id, "code.rust.async", "{}", sel.to_json());
        let t = &sel.ranked[0].terms;
        for term in ["vec", "lex", "route", "prior", "pin"] {
            assert!(t.get(term).is_some(), "{term}");
        }
        assert_eq!(t["vec"]["used"], true);
        assert!(t["vec"]["raw"].as_f64().unwrap() > 0.2);
        assert!(t["lex"]["raw"].as_f64().unwrap() > 0.0);
        assert_eq!(t["route"]["value"], 0.0);
        let sum = t["vec"]["value"].as_f64().unwrap()
            + t["lex"]["value"].as_f64().unwrap()
            + t["prior"]["value"].as_f64().unwrap()
            + t["pin"]["value"].as_f64().unwrap();
        assert!(
            (sum - sel.ranked[0].score).abs() < 1e-9,
            "the score is the sum of the recorded terms"
        );
        assert!(sel.notes.iter().any(|n| n.contains("router")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn prior_and_pin_move_the_score_by_exactly_their_amount() {
        let (home, s) = setup();
        let src = home.join("src");
        add(
            &s,
            &sample::Builder::new(&src, "adapter", "a.one", "1.0.0")
                .descriptor("same text")
                .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
                .build(),
        );
        add(
            &s,
            &sample::Builder::new(&src, "adapter", "a.two", "1.0.0")
                .descriptor("same text")
                .prior(0.75)
                .file("payload/a.gguf", "lora", "gguf", vec![2; 64])
                .build(),
        );
        let base = rank(&s, &query("same text"), &weights(), Budget::default(), qv("same text")).unwrap();
        let by = |sel: &Selection, id: &str| sel.ranked.iter().find(|r| r.record.id == id).unwrap().score;
        let d = by(&base, "a.two") - by(&base, "a.one");
        assert!((d - 0.75).abs() < 1e-6, "{d}");
        s.set_pinned("a.one", true, None).unwrap();
        let pinned = rank(&s, &query("same text"), &weights(), Budget::default(), qv("same text")).unwrap();
        assert!((by(&pinned, "a.one") - by(&base, "a.one") - 2.0).abs() < 1e-9);
        assert_eq!(pinned.ranked[0].record.id, "a.one", "2.0 pin beats 0.75 prior");
        assert_eq!(pinned.ranked[0].terms["pin"]["pinned"], true);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_embed_model_mismatch_guard_drops_the_vector_term_and_says_so() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &sample::adapter(&src, "code.rust", "1.0.0", "rust tokio"));
        add(
            &s,
            &sample::Builder::new(&src, "adapter", "code.other", "1.0.0")
                .descriptor("rust tokio")
                .embed_model("mm-embed-1")
                .file("payload/a.gguf", "lora", "gguf", vec![3; 64])
                .build(),
        );
        let sel = rank(
            &s,
            &query("rust tokio"),
            &weights(),
            Budget::default(),
            qv("rust tokio"),
        )
        .unwrap();
        let other = sel.ranked.iter().find(|r| r.record.id == "code.other").unwrap();
        assert_eq!(other.terms["vec"]["used"], false);
        assert_eq!(other.terms["vec"]["value"], 0.0);
        assert!(
            other
                .notes
                .iter()
                .any(|n| n.contains("mm-embed-1") && n.contains("hash-384")),
            "{:?}",
            other.notes
        );
        let ok = sel.ranked.iter().find(|r| r.record.id == "code.rust").unwrap();
        assert_eq!(ok.terms["vec"]["used"], true);
        assert!(ok.score > other.score);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_failed_embedder_leaves_the_lexical_term_and_a_note() {
        let (home, s) = setup();
        library(&home, &s);
        let sel = rank(
            &s,
            &query("sql joins"),
            &weights(),
            Budget::default(),
            Err("no model".into()),
        )
        .unwrap();
        assert_eq!(sel.ranked[0].record.id, "code.sql");
        assert!(sel.notes.iter().any(|n| n.contains("embedder failed")));
        assert_eq!(sel.ranked[0].terms["vec"]["used"], false);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn filters_exclude_with_reasons() {
        let (home, s) = setup();
        library(&home, &s);
        let src = home.join("src");
        add(
            &s,
            &Builder::new(&src, "adapter", "code.misty", "1.0.0")
                .descriptor("misty only")
                .base("misty")
                .file("payload/a.gguf", "lora", "gguf", vec![4; 64])
                .build(),
        );
        add(
            &s,
            &Builder::new(&src, "adapter", "code.big", "1.0.0")
                .descriptor("huge")
                .declared(1 << 40, 1 << 40)
                .file("payload/a.gguf", "lora", "gguf", vec![5; 64])
                .build(),
        );
        add(
            &s,
            &Builder::new(&src, "adapter", "vision.cap", "1.0.0")
                .descriptor("picture")
                .input(&["image"])
                .file("payload/a.gguf", "lora", "gguf", vec![6; 64])
                .build(),
        );
        s.set_banned("code.sql", true, Some("wrong answers")).unwrap();
        let q = Query {
            base: Some("dusty".into()),
            base_version: Some(Version::new(2, 0, 0)),
            arch: Some("qwen35moe".into()),
            input: vec!["text".into()],
            ..query("anything")
        };
        let budget = Budget {
            ram: 1 << 30,
            vram: 1 << 30,
        };
        let sel = rank(&s, &q, &weights(), budget, qv("anything")).unwrap();
        let why = |key: &str| {
            sel.excluded
                .iter()
                .find(|e| e.key == key)
                .map(|e| e.reasons.join(" | "))
                .unwrap_or_default()
        };
        assert!(
            why("code.sql@1.0.0").contains("banned by the owner (wrong answers)"),
            "{}",
            why("code.sql@1.0.0")
        );
        assert!(why("code.misty@1.0.0").contains("built for base misty, not dusty"));
        assert!(why("code.big@1.0.0").contains("can never fit"));
        assert!(why("vision.cap@1.0.0").contains("does not take text input"));
        // the sample adapters want base_version >=1.0, <2.0 and the core is 2.0.0
        assert!(
            why("code.rust.async@1.0.0").contains("needs base version"),
            "{}",
            why("code.rust.async@1.0.0")
        );
        let q2 = Query {
            base_version: Some(Version::new(1, 5, 0)),
            ..q
        };
        let sel2 = rank(&s, &q2, &weights(), budget, qv("anything")).unwrap();
        assert!(sel2.ranked.iter().any(|r| r.record.id == "code.rust.async"));
        assert!(sel2.excluded.iter().all(|e| !e.key.starts_with("code.rust.async")));
        // docs.cooking is base any and took no base/arch conditions
        assert!(sel2.ranked.iter().any(|r| r.record.id == "docs.cooking"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn misdeclared_kms_are_not_selected() {
        let (home, s) = setup();
        library(&home, &s);
        s.set_misdeclared("code.sql@1.0.0", Some("measured 3x the declared RAM"))
            .unwrap();
        let sel = rank(&s, &query("sql"), &weights(), Budget::default(), qv("sql")).unwrap();
        assert!(sel.ranked.iter().all(|r| r.record.id != "code.sql"));
        assert!(sel
            .excluded
            .iter()
            .any(|e| e.key == "code.sql@1.0.0" && e.reasons[0].starts_with("misdeclared")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tag_and_domain_queries_are_exact() {
        let (home, s) = setup();
        library(&home, &s);
        let sel = rank(
            &s,
            &query("tag:lang:rust domain:code/*"),
            &weights(),
            Budget::default(),
            Ok(None),
        )
        .unwrap();
        let ids: Vec<&str> = sel.ranked.iter().map(|r| r.record.id.as_str()).collect();
        assert_eq!(ids.len(), 2, "both adapters are code/rust with lang:rust: {ids:?}");
        assert!(ids.contains(&"code.rust.async") && !ids.contains(&"docs.cooking"));
        let sel = rank(
            &s,
            &query("tag:lang:rust domain:docs/*"),
            &weights(),
            Budget::default(),
            Ok(None),
        )
        .unwrap();
        assert!(sel.ranked.is_empty());
        assert!(sel
            .excluded
            .iter()
            .any(|e| e.key == "docs.cooking@1.0.0" && e.reasons.iter().any(|r| r.contains("missing tag lang:rust"))));
        let sel = rank(&s, &query("kind:memory"), &weights(), Budget::default(), Ok(None)).unwrap();
        assert_eq!(sel.ranked.len(), 1);
        assert_eq!(sel.ranked[0].record.kind, "memory");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn versions_come_from_the_channel_else_the_newest() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &sample::adapter(&src, "code.rust", "1.0.0", "rust"));
        add(&s, &sample::adapter(&src, "code.rust", "2.0.0", "rust"));
        let sel = rank(&s, &query("rust"), &weights(), Budget::default(), Ok(None)).unwrap();
        assert_eq!(sel.ranked.len(), 1);
        assert_eq!(sel.ranked[0].record.version, "2.0.0");
        assert!(sel.ranked[0].via.contains("newest"));
        s.channel_set("code.rust", "stable", "1.0.0", None).unwrap();
        let sel = rank(&s, &query("rust"), &weights(), Budget::default(), Ok(None)).unwrap();
        assert_eq!(sel.ranked[0].record.version, "1.0.0");
        assert_eq!(sel.ranked[0].via, "channel stable");
        let all = rank(
            &s,
            &Query {
                all_versions: true,
                ..query("rust")
            },
            &weights(),
            Budget::default(),
            Ok(None),
        )
        .unwrap();
        assert_eq!(all.ranked.len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn absorbed_kms_are_skipped_unless_asked_for() {
        let (home, s) = setup();
        let src = home.join("src");
        add(
            &s,
            &Builder::new(&src, "adapter", "skill.done", "1.0.0")
                .descriptor("learned already")
                .tables("[consolidation]\nstate = \"absorbed\"\nuplift = 0.01\nsteps = 12")
                .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
                .build(),
        );
        add(&s, &Builder::new(&src, "adapter", "skill.going", "1.0.0").descriptor("learning now")
            .tables("[consolidation.dusty]\nstate = \"learning\"\nuplift = 0.2\nsteps = 3\n[consolidation.misty]\nstate = \"absorbed\"")
            .file("payload/a.gguf", "lora", "gguf", vec![2; 64]).build());
        let sel = rank(&s, &query("learned"), &weights(), Budget::default(), Ok(None)).unwrap();
        assert!(sel.ranked.iter().all(|r| r.record.id != "skill.done"));
        let why = &sel
            .excluded
            .iter()
            .find(|e| e.key == "skill.done@1.0.0")
            .unwrap()
            .reasons[0];
        assert!(
            why.contains("absorbed into dusty's weights") && why.contains("include_absorbed"),
            "{why}"
        );
        // per core: learning for dusty, absorbed for misty
        let dusty = Query {
            base: Some("dusty".into()),
            ..query("learning")
        };
        assert!(rank(&s, &dusty, &weights(), Budget::default(), Ok(None))
            .unwrap()
            .ranked
            .iter()
            .any(|r| r.record.id == "skill.going"));
        let misty = Query {
            base: Some("misty".into()),
            ..query("learning")
        };
        assert!(rank(&s, &misty, &weights(), Budget::default(), Ok(None))
            .unwrap()
            .excluded
            .iter()
            .any(|e| e.key == "skill.going@1.0.0"));
        let all = Query {
            include_absorbed: true,
            ..query("learned")
        };
        assert!(rank(&s, &all, &weights(), Budget::default(), Ok(None))
            .unwrap()
            .ranked
            .iter()
            .any(|r| r.record.id == "skill.done"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn tags_are_weighed_by_confidence() {
        let (home, s) = setup();
        let src = home.join("src");
        for (id, conf) in [("t.sure", 0.95), ("t.unsure", 0.4)] {
            add(
                &s,
                &Builder::new(&src, "adapter", id, "1.0.0")
                    .descriptor("same words")
                    .tags(&["lang:rust"])
                    .top(&format!(
                        "[tag_info]\n\"lang:rust\" = {{ origin = \"classifier\", confidence = {conf} }}"
                    ))
                    .file("payload/a.gguf", "lora", "gguf", vec![3; 64])
                    .build(),
            );
        }
        let q = query("tag:lang:rust same words");
        let sel = rank(&s, &q, &weights(), Budget::default(), Ok(None)).unwrap();
        assert_eq!(sel.ranked[0].record.id, "t.sure");
        let diff = sel.ranked[0].score - sel.ranked[1].score;
        assert!((diff - 0.5 * (0.95 - 0.4)).abs() < 1e-9, "{diff}");
        assert_eq!(sel.ranked[1].terms["tags"]["matched"], 1);
        // a confidence floor excludes the uncertain one, with the origin in the reason
        let q = Query {
            min_tag_confidence: 0.8,
            ..q
        };
        let sel = rank(&s, &q, &weights(), Budget::default(), Ok(None)).unwrap();
        assert_eq!(sel.ranked.len(), 1);
        let why = &sel.excluded[0].reasons[0];
        assert!(why.contains("confidence 0.40 (classifier)"), "{why}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn top_k_limits_the_list() {
        let (home, s) = setup();
        library(&home, &s);
        let sel = rank(
            &s,
            &Query {
                top_k: 1,
                ..query("rust")
            },
            &weights(),
            Budget::default(),
            Ok(None),
        )
        .unwrap();
        assert_eq!(sel.ranked.len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }
}

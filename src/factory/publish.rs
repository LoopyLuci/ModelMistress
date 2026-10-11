//! Step 8, publish: every KM of the task goes to the `candidate` channel, and the promotion policy decides the rest
//! (`architecture.md` section 5):
//!
//! * `off`: nothing is published;
//! * `propose`: candidate, and a promotion item waits in the review queue for the owner;
//! * `gated` (the default): candidate, promoted to `stable` only if the evaluation shows an uplift of at least
//!   `min_uplift`, it beats the current stable version's recorded uplift, and no review item of the task is open;
//! * `auto`: candidate, then promoted at once.
//!
//! A channel move is one row (`km.channel.set`), so a rollback is moving it back.

use serde_json::{json, Value};

use super::run::{Run, StepRes};
use super::StepEnd;
use crate::km::manifest::Manifest;

/// The newest version of each KM id this task made (the evaluated one if there is one).
pub fn finals(run: &Run) -> Vec<(String, String, String)> {
    let arts = run.f.db.artifacts(&run.id);
    let mut out: Vec<(String, String, String)> = vec![];
    for a in arts.iter().filter(|a| a.imported) {
        match out.iter_mut().find(|o| o.0 == a.id) {
            Some(o) => {
                let better = semver::Version::parse(&a.version).ok() > semver::Version::parse(&o.1).ok();
                if better {
                    o.1 = a.version.clone();
                }
            }
            None => out.push((a.id.clone(), a.version.clone(), a.kind.clone())),
        }
    }
    out
}

fn recorded_uplift(run: &Run, id: &str, version: &str) -> Option<f64> {
    let dir = run.km.store.dir_of(&format!("{id}@{version}"));
    Manifest::from_file(&dir.join("km.toml"))
        .ok()?
        .quality
        .scores
        .get("uplift")
        .copied()
}

/// Promotes `id@version` to stable; returns what the channel pointed at before.
pub fn promote(run: &Run, id: &str, version: &str) -> Result<Option<String>, crate::engine::Fail> {
    run.km.store.channel_set(id, "stable", version, None)
}

pub async fn run(run: &Run) -> StepRes<StepEnd> {
    let s = run.f.settings();
    let policy = run.spec.approval.clone().unwrap_or_else(|| s.promotion.clone());
    let kms = finals(run);
    if policy == "off" {
        return Ok(StepEnd::Done(
            json!({"policy": policy, "published": [], "note": "promotion policy is off: nothing was published"}),
        ));
    }
    let mut published = vec![];
    for (id, version, _) in &kms {
        run.check()?;
        let prev = run.km.store.channel_set(id, "candidate", version, None)?;
        published.push(json!({"id": id, "version": version, "channel": "candidate", "previous": prev}));
    }
    let uplift = run
        .f
        .db
        .artifacts(&run.id)
        .iter()
        .find_map(|a| a.draft["scores"]["uplift"].as_f64())
        .or_else(|| kms.iter().find_map(|(id, v, _)| recorded_uplift(run, id, v)));
    let open = run.f.db.review_open_count(&run.id);
    let mut promoted: Vec<Value> = vec![];
    let mut held_back: Vec<String> = vec![];
    match policy.as_str() {
        "auto" => {
            for (id, version, kind) in &kms {
                if kind == "adapter" {
                    continue;
                }
                promote(run, id, version)?;
                promoted.push(json!({"id": id, "version": version}));
            }
        }
        "propose" => {
            let keys: Vec<String> = kms.iter().map(|(i, v, _)| format!("{i}@{v}")).collect();
            run.f.db.add_review(
                &run.id,
                "promotion",
                &run.id,
                "the task's KMs are candidates; promote them to stable?",
                &json!({"kms": keys, "uplift": uplift}),
            )?;
            held_back.push("waiting for the owner's decision (propose)".into());
        }
        _ => {
            // gated
            let mut reasons = vec![];
            match uplift {
                None => reasons.push("no evaluation: there is no uplift to judge by".to_string()),
                Some(u) if u < s.min_uplift => {
                    reasons.push(format!("uplift {u:.3} is below the required {:.3}", s.min_uplift))
                }
                _ => {}
            }
            if open > 0 {
                reasons.push(format!("{open} review items are still open"));
            }
            for (id, version, kind) in &kms {
                if kind == "adapter" {
                    continue;
                }
                let stable = run.km.store.channel_get(id, "stable");
                if let (Some(st), Some(u)) = (stable.as_deref().and_then(|v| recorded_uplift(run, id, v)), uplift) {
                    if st >= u {
                        reasons.push(format!(
                            "{id}: the stable version's uplift {st:.3} is not beaten ({u:.3})"
                        ));
                    }
                }
                let _ = version;
            }
            if reasons.is_empty() {
                for (id, version, kind) in &kms {
                    if kind != "adapter" {
                        promote(run, id, version)?;
                        promoted.push(json!({"id": id, "version": version}));
                    }
                }
            } else {
                held_back = reasons;
            }
        }
    }
    Ok(StepEnd::Done(
        json!({"policy": policy, "published": published, "promoted": promoted, "held_back": held_back,
                            "uplift": uplift, "open_review_items": open}),
    ))
}

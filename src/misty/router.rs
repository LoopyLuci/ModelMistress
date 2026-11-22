//! The gate: which role handles a request. Pure code, no model calls, so every decision can be printed and tested.
//!
//! The rule: the cheapest brain whose score clears its bar, with a bonus for a model that is already loaded (a cold
//! 27B load costs more than a slightly weaker answer from a warm 9B), audio and images going to their experts first,
//! and explicit overrides (`@fast`, `@main`, `@deep`) winning over everything.

use std::collections::BTreeMap;

use serde::Serialize;

use super::roster::Role;

/// What the router knows about a request. Built by [`Features::of`] from text, filled in by the caller for the rest.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Features {
    pub tokens: usize,
    pub has_image: bool,
    pub has_audio: bool,
    pub code: f32,
    pub reasoning: f32,
    pub lookup: f32,
    pub chitchat: f32,
    pub tool_hint: f32,
    /// Strength of the best retrieval hit, 0 to 1 (0 when there is no index or no hit).
    pub rag_hit: f32,
    pub turns: usize,
    /// `@fast` / `@main` / `@deep` in the text, or a model id such as `misty-deep`.
    pub forced: Option<Role>,
}

const CODE_WORDS: &[&str] = &[
    "```",
    "function",
    "compile",
    "stack trace",
    "traceback",
    "exception",
    "bug",
    "refactor",
    "regex",
    "sql",
    "rust",
    "python",
    "javascript",
    "typescript",
    "struct",
    "borrow checker",
    "unit test",
    "segfault",
    "def ",
    "fn ",
    "class ",
];
const REASON_WORDS: &[&str] = &[
    "prove",
    "derive",
    "step by step",
    "analyze",
    "analyse",
    "trade-off",
    "tradeoff",
    "compare",
    "design",
    "architecture",
    "optimi",
    "why does",
    "why is",
    "explain in depth",
    "pros and cons",
    "plan",
    "strategy",
    "evaluate",
    "theorem",
    "complexity",
    "algorithm",
    "reason about",
    "root cause",
];
const LOOKUP_WORDS: &[&str] = &[
    "what is",
    "who is",
    "when did",
    "define ",
    "meaning of",
    "capital of",
    "how many",
    "translate",
];
const CHAT_WORDS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "thanks",
    "thank you",
    "good morning",
    "good night",
    "how are you",
    "ok",
    "okay",
];
const TOOL_WORDS: &[&str] = &[
    "file",
    "folder",
    "directory",
    "read ",
    "list ",
    "search ",
    "open ",
    "ingest",
    "index",
    "load the model",
    "unload",
    "fetch",
    "download",
    "run ",
    "what time",
    "calculate",
    "my notes",
    "my documents",
];

fn score(text: &str, words: &[&str], per_hit: f32) -> f32 {
    (words.iter().filter(|w| text.contains(*w)).count() as f32 * per_hit).min(1.0)
}

/// `@fast`, `@main` or `@deep` anywhere in the text: the role it names, and the text without it.
pub fn forced_role(text: &str) -> (Option<Role>, String) {
    let mut forced = None;
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|w| match w.strip_prefix('@').and_then(Role::parse) {
            Some(r) if r.is_brain() => {
                forced = Some(r);
                false
            }
            _ => true,
        })
        .collect();
    (forced, kept.join(" "))
}

/// The role a model id such as `misty-deep` names (`misty` and `misty-main` name none: the router decides).
pub fn role_of_model(id: &str) -> Option<Role> {
    match id.trim().to_lowercase().strip_prefix("misty-") {
        Some("fast") => Some(Role::Fast),
        Some("deep") => Some(Role::Deep),
        Some("main") => Some(Role::Main),
        _ => None,
    }
}

impl Features {
    pub fn of(text: &str) -> Features {
        let (forced, clean) = forced_role(text);
        let lower = clean.to_lowercase();
        let tokens = clean.chars().count() / 4 + 1;
        let words = clean.split_whitespace().count();
        let short = words <= 6;
        Features {
            tokens,
            code: score(&lower, CODE_WORDS, 0.35),
            reasoning: (score(&lower, REASON_WORDS, 0.35) + if tokens > 400 { 0.2 } else { 0.0 }).min(1.0),
            lookup: if words <= 14 {
                score(&lower, LOOKUP_WORDS, 0.6)
            } else {
                0.0
            },
            chitchat: if short
                && CHAT_WORDS.iter().any(|w| {
                    lower.trim_matches(|c: char| !c.is_alphanumeric()) == *w || lower.starts_with(&format!("{w} "))
                }) {
                1.0
            } else {
                0.0
            },
            // Arithmetic names no tool, but a small model gets it wrong without the calculator.
            tool_hint: score(&lower, TOOL_WORDS, 0.4).max(if super::agent::is_arithmetic(&lower) { 0.4 } else { 0.0 }),
            forced,
            ..Default::default()
        }
    }
}

/// The router's view of the machine.
#[derive(Clone, Debug, Default)]
pub struct Fleet {
    /// Brain roles that resolve to a model that is ready.
    pub ready: Vec<Role>,
    /// Roles whose model is loaded right now.
    pub warm: Vec<Role>,
    /// The Cactus roles that can run (`reflex`, `ear`, `eye`).
    pub cactus: Vec<Role>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Route {
    /// The role that answers. `None` only when nothing can: the caller reports the roster.
    pub role: Option<Role>,
    pub scores: BTreeMap<String, f32>,
    pub reasons: Vec<String>,
    /// Retrieve before answering.
    pub rag: bool,
    /// Offer tools to the model.
    pub tools: bool,
    /// Run the Needle shortcut first.
    pub reflex: bool,
    /// Where to go if the answer looks weak.
    pub escalate_to: Option<Role>,
    /// Audio goes to Whistle first, images to Mirage (unless the brain sees images itself).
    pub pre: Vec<Role>,
}

/// Decide. `panel` is the owner's setting; it only allows the panel, the agent decides when to use it.
pub fn route(f: &Features, fleet: &Fleet, reflex_enabled: bool) -> Route {
    let mut reasons = vec![];
    let mut scores: BTreeMap<String, f32> = BTreeMap::new();
    let mut pre = vec![];
    if f.has_audio {
        if fleet.cactus.contains(&Role::Ear) {
            pre.push(Role::Ear);
            reasons.push("audio goes to Whistle first".to_string());
        } else {
            reasons.push("audio given but Whistle is not installed".to_string());
        }
    }
    if f.has_image {
        if fleet.cactus.contains(&Role::Eye) {
            pre.push(Role::Eye);
            reasons.push("an image goes to Mirage first".to_string());
        } else {
            reasons.push("an image given but Mirage is not installed".to_string());
        }
    }

    let warm = |r: Role| if fleet.warm.contains(&r) { 0.10 } else { 0.0 };
    let long = (f.tokens as f32 / 6000.0).min(1.0);
    let raw = [
        (
            Role::Fast,
            0.45 + 0.4 * f.lookup + 0.5 * f.chitchat
                - 0.55 * f.reasoning
                - 0.30 * f.code
                - 0.5 * long
                - 0.2 * f.rag_hit
                - 0.15 * f.tool_hint,
        ),
        (
            Role::Main,
            0.55 + 0.15 * f.code + 0.15 * f.reasoning + 0.15 * f.tool_hint + 0.1 * f.rag_hit,
        ),
        (Role::Deep, -0.05 + 0.9 * f.reasoning + 0.30 * f.code + 0.35 * long),
    ];
    for (r, s) in raw {
        let s = s + warm(r);
        scores.insert(r.name().to_string(), (s * 100.0).round() / 100.0);
    }

    let candidates: Vec<Role> = [Role::Fast, Role::Main, Role::Deep]
        .into_iter()
        .filter(|r| fleet.ready.contains(r))
        .collect();
    let mut role = None;
    if let Some(forced) = f.forced {
        if candidates.contains(&forced) {
            role = Some(forced);
            reasons.push(format!("you asked for {}", forced.name()));
        } else {
            reasons.push(format!(
                "you asked for {} but it is not available here; routing normally",
                forced.name()
            ));
        }
    }
    if role.is_none() {
        // the highest score wins; a tie goes to the cheaper role
        role = candidates.iter().copied().max_by(|a, b| {
            let (sa, sb) = (scores[a.name()], scores[b.name()]);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal).then(b.cmp(a))
        });
        if let Some(r) = role {
            reasons.push(format!("{} scored highest ({})", r.name(), scores[r.name()]));
            if fleet.warm.contains(&r) {
                reasons.push(format!("{} is already loaded", r.name()));
            }
        } else {
            reasons.push("no language model is ready: see the roster".to_string());
        }
    }

    let rag = f.rag_hit >= 0.25 || f.lookup > 0.0 && f.rag_hit > 0.0 || f.tool_hint >= 0.4 && f.rag_hit > 0.0;
    if rag {
        reasons.push(format!("retrieval looks relevant (hit {:.2})", f.rag_hit));
    }
    let tools = f.tool_hint > 0.0 || f.code > 0.0 && f.tool_hint > 0.0 || f.turns > 0 && f.tool_hint > 0.0;
    let reflex = reflex_enabled
        && fleet.cactus.contains(&Role::Reflex)
        && f.tool_hint >= 0.4
        && f.reasoning < 0.35
        && !f.has_audio;
    if reflex {
        reasons.push("a tool is implied: Needle tries first".to_string());
    }
    let escalate_to = role.and_then(|r| r.up()).filter(|u| candidates.contains(u));
    Route {
        role,
        scores,
        reasons,
        rag,
        tools,
        reflex,
        escalate_to,
        pre,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet() -> Fleet {
        Fleet {
            ready: vec![Role::Fast, Role::Main, Role::Deep],
            warm: vec![],
            cactus: vec![Role::Reflex, Role::Ear, Role::Eye],
        }
    }

    fn go(text: &str, fleet: &Fleet) -> Route {
        route(&Features::of(text), fleet, true)
    }

    #[test]
    fn greetings_and_lookups_go_to_the_fast_role() {
        assert_eq!(go("hello", &fleet()).role, Some(Role::Fast));
        assert_eq!(go("what is the capital of France?", &fleet()).role, Some(Role::Fast));
    }

    #[test]
    fn ordinary_work_goes_to_main_and_hard_reasoning_to_deep() {
        assert_eq!(
            go(
                "Summarize the main differences between these two release notes and tell me what to update first",
                &fleet()
            )
            .role,
            Some(Role::Main)
        );
        let hard = "Analyze the trade-off between a B-tree and an LSM tree for this workload, derive the write amplification step by step and compare the algorithm complexity, then design a plan to evaluate it";
        assert_eq!(go(hard, &fleet()).role, Some(Role::Deep));
    }

    #[test]
    fn a_forced_role_wins_and_is_removed_from_the_text() {
        let (r, text) = forced_role("@deep why is the sky blue @now");
        assert_eq!(r, Some(Role::Deep));
        assert_eq!(text, "why is the sky blue @now");
        assert_eq!(go("hello @deep", &fleet()).role, Some(Role::Deep));
        assert_eq!(role_of_model("misty-fast"), Some(Role::Fast));
        assert_eq!(role_of_model("misty"), None);
    }

    #[test]
    fn a_forced_role_that_is_not_installed_falls_back_and_says_so() {
        let mut f = fleet();
        f.ready = vec![Role::Fast, Role::Main];
        let r = go("hello @deep", &f);
        assert_ne!(r.role, Some(Role::Deep));
        assert!(r.reasons.iter().any(|x| x.contains("not available")));
    }

    #[test]
    fn a_warm_model_beats_a_slightly_better_cold_one() {
        let text = "Summarize the notes and compare the options for me";
        let cold = go(text, &fleet());
        let mut f = fleet();
        f.warm = vec![Role::Fast];
        let warm = go(text, &f);
        assert!(warm.scores["fast"] > cold.scores["fast"]);
    }

    #[test]
    fn nothing_ready_is_reported_not_guessed() {
        let r = go("hello", &Fleet::default());
        assert_eq!(r.role, None);
        assert!(r.reasons.iter().any(|x| x.contains("roster")));
    }

    #[test]
    fn media_goes_to_its_expert_first_or_says_it_cannot() {
        let mut ft = Features::of("what is in this picture?");
        ft.has_image = true;
        ft.has_audio = true;
        let r = route(&ft, &fleet(), true);
        assert_eq!(r.pre, vec![Role::Ear, Role::Eye]);
        let none = Fleet {
            ready: vec![Role::Main],
            ..Default::default()
        };
        let r = route(&ft, &none, true);
        assert!(r.pre.is_empty());
        assert!(r.reasons.iter().any(|x| x.contains("Mirage is not installed")));
    }

    #[test]
    fn tool_requests_try_the_reflex_and_offer_tools_and_reasoning_does_not() {
        let r = go("list the files in my notes folder", &fleet());
        assert!(r.reflex && r.tools);
        let r = route(&Features::of("list the files in my notes folder"), &fleet(), false);
        assert!(!r.reflex, "the setting turns the shortcut off");
        let r = go("prove the theorem and derive it step by step", &fleet());
        assert!(!r.reflex);
        // a sum names no tool, yet it is a tool request: the model is wrong without the calculator
        for sum in ["what is 17 times 23", "what is 17 * 23", "what's 15% of 240"] {
            let r = go(sum, &fleet());
            assert!(r.tools && r.reflex, "{sum}");
        }
        let r = go("how many times did we meet over three days", &fleet());
        assert!(!r.tools, "'times' and 'over' with no numbers are not a sum");
        let no_needle = Fleet {
            cactus: vec![],
            ..fleet()
        };
        assert!(!go("list the files in my notes folder", &no_needle).reflex);
    }

    #[test]
    fn retrieval_is_used_when_the_index_has_a_strong_hit() {
        let mut f = Features::of("what did we decide about the release date");
        f.rag_hit = 0.6;
        assert!(route(&f, &fleet(), true).rag);
        f.rag_hit = 0.0;
        assert!(!route(&f, &fleet(), true).rag);
    }

    #[test]
    fn escalation_goes_up_one_role_and_only_to_a_ready_one() {
        let r = go("Summarize the notes and compare the options for me", &fleet());
        assert_eq!(r.escalate_to, r.role.and_then(|x| x.up()));
        let mut f = fleet();
        f.ready = vec![Role::Fast, Role::Main];
        let r = route(&Features::of("hello @main"), &f, true);
        assert_eq!(r.escalate_to, None, "deep is not ready");
    }
}

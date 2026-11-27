//! The owner's verdicts on Misty's replies. A verdict says whether a reply was right, whether it should have used a tool, or
//! what the owner says is right instead. Misty reads them: a question like one the owner taught is routed as he taught it,
//! and a question like one he corrected is answered with the correction in mind. The verdicts can also be exported as
//! chat-format training data.
//!
//! The rules match the Android app (`core/misty/MistyFeedback.kt` and `MistyRouter.kt`), so a verdict means the same on both.
//! The state database keeps them (`state.rs`, table `feedback`).

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// How much two questions must share, from 0 to 1, before a verdict on one applies to the other.
pub const SIMILAR: f64 = 0.6;

/// The newest verdicts kept. Older ones are dropped when a new one is saved.
pub const CAPACITY: usize = 500;

/// A reply kept with a verdict is cut to this many characters.
pub const MAX_ANSWER_CHARS: usize = 1000;

/// A correction note shows at most this many characters of the wrong reply.
const NOTE_ANSWER_CHARS: usize = 300;

/// Corrections given to the model for one question, at most.
const NOTE_LIMIT: usize = 2;

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "it", "to", "of", "in", "on", "for", "and", "or", "my", "me", "i", "you", "do", "does",
    "can", "please", "what", "that", "this", "with",
];

/// The owner's verdict on one reply. The wire names match the Android app's export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// The reply was right. Similar questions keep the tools that were offered with it.
    Good,
    /// The reply was wrong. Kept for review; it does not change routing.
    Bad,
    /// The reply should have used no tool. Similar questions are answered with none offered.
    NoTools,
    /// The reply should have used a tool. Similar questions are offered tools.
    NeedsTools,
    /// The owner says what is right instead. Similar questions are answered with it in mind; routing does not change.
    Corrected,
}

impl Label {
    pub fn wire(self) -> &'static str {
        match self {
            Label::Good => "good",
            Label::Bad => "bad",
            Label::NoTools => "no_tools",
            Label::NeedsTools => "needs_tools",
            Label::Corrected => "corrected",
        }
    }

    pub fn of(wire: &str) -> Option<Label> {
        [Label::Good, Label::Bad, Label::NoTools, Label::NeedsTools, Label::Corrected]
            .into_iter()
            .find(|l| l.wire() == wire)
    }

    /// The verdicts that say what tools a question needs. A good or a bad reply says nothing about tools, so it routes nothing.
    fn routes(self) -> bool {
        matches!(self, Label::NoTools | Label::NeedsTools)
    }
}

/// One saved verdict.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub question: String,
    pub answer: String,
    pub label: Label,
    /// The tools that were offered with the reply, by name.
    pub tools: Vec<String>,
    /// What the owner says is right. Set for `Corrected` only.
    pub correction: Option<String>,
    /// Unix seconds.
    pub at: i64,
}

impl Verdict {
    /// Checks a verdict from the owner before it is saved. The reply is cut to [`MAX_ANSWER_CHARS`]. A correction is kept
    /// for `Corrected` only, and that label needs one.
    pub fn new(
        question: &str,
        answer: &str,
        label: Label,
        tools: Vec<String>,
        correction: Option<&str>,
        at: i64,
    ) -> Result<Verdict, String> {
        let question = question.trim();
        if question.is_empty() {
            return Err("the question is empty".into());
        }
        let correction = correction.map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
        if label == Label::Corrected && correction.is_none() {
            return Err("a correction needs the correct answer".into());
        }
        Ok(Verdict {
            question: question.to_string(),
            answer: cut(answer.trim(), MAX_ANSWER_CHARS),
            label,
            tools,
            correction: if label == Label::Corrected { correction } else { None },
            at,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "question": self.question,
            "answer": self.answer,
            "label": self.label.wire(),
            "tools": self.tools,
            "correction": self.correction,
            "at": self.at,
        })
    }
}

/// The current time in unix seconds.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// The words of `text` that carry its meaning: lower case, no punctuation, no small common words.
pub fn words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .map(|w| w.trim_matches('.'))
        .filter(|w| w.chars().count() > 1 && !STOP_WORDS.iter().any(|s| s == w))
        .map(str::to_string)
        .collect()
}

/// The share of meaningful words two questions have in common, from 0 to 1.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (left, right) = (words(a), words(b));
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    left.intersection(&right).count() as f64 / left.union(&right).count() as f64
}

/// The verdicts that `keep` accepts and that are close enough to `question`: the most similar first, and the newest first
/// among equals.
fn similar<'a>(question: &str, all: &'a [Verdict], keep: impl Fn(&Verdict) -> bool) -> Vec<&'a Verdict> {
    let mut found: Vec<(&Verdict, f64)> = all
        .iter()
        .filter(|v| keep(v))
        .map(|v| (v, similarity(question, &v.question)))
        .filter(|(_, s)| *s >= SIMILAR)
        .collect();
    found.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal).then(b.0.at.cmp(&a.0.at)));
    found.into_iter().map(|(v, _)| v).collect()
}

/// The verdict that applies to `question` when it is routed: the most similar routing verdict that is close enough. A
/// correction never applies here.
pub fn taught<'a>(question: &str, all: &'a [Verdict]) -> Option<&'a Verdict> {
    similar(question, all, |v| v.label.routes()).into_iter().next()
}

/// The owner's corrections that apply to `question`, the most similar first, at most `limit`.
pub fn corrections<'a>(question: &str, all: &'a [Verdict], limit: usize) -> Vec<&'a Verdict> {
    let mut found = similar(question, all, |v| {
        v.label == Label::Corrected && v.correction.as_deref().is_some_and(|c| !c.trim().is_empty())
    });
    found.truncate(limit);
    found
}

/// The note given to the model with a question the owner corrected before: what was wrong, and what is right. None when
/// no correction applies.
pub fn correction_note(question: &str, all: &[Verdict]) -> Option<String> {
    let found = corrections(question, all, NOTE_LIMIT);
    if found.is_empty() {
        return None;
    }
    let mut note = String::from(
        "Notes from the owner about similar questions. Follow the correction, and do not repeat the wrong reply:",
    );
    for v in found {
        note.push_str(&format!(
            "\n- Asked before: {}\n  Reply given then, which was wrong: {}\n  Correct answer: {}",
            v.question,
            cut(&v.answer, NOTE_ANSWER_CHARS),
            v.correction.as_deref().unwrap_or("").trim(),
        ));
    }
    Some(note)
}

/// The verdicts as chat-format training data, one JSON object per line: each good reply, and each correction with the
/// correct answer as the assistant's reply. The wrong reply is kept in the metadata of a correction.
pub fn training_lines(all: &[Verdict]) -> Vec<String> {
    all.iter()
        .filter_map(|v| {
            let (answer, rejected) = match (v.label, v.correction.as_deref()) {
                (Label::Good, _) if !v.answer.trim().is_empty() => (v.answer.clone(), None),
                (Label::Corrected, Some(c)) if !c.trim().is_empty() => (c.trim().to_string(), Some(v.answer.clone())),
                _ => return None,
            };
            let mut metadata = json!({"label": v.label.wire(), "tools": v.tools, "at": v.at});
            if let Some(wrong) = rejected {
                metadata["rejected"] = json!(wrong);
            }
            Some(
                json!({
                    "messages": [
                        {"role": "user", "content": v.question},
                        {"role": "assistant", "content": answer},
                    ],
                    "metadata": metadata,
                })
                .to_string(),
            )
        })
        .collect()
}

/// How many verdicts of each kind are saved, and how many of them can be exported as training data.
pub fn counts(all: &[Verdict]) -> Value {
    let n = |label: Label| all.iter().filter(|v| v.label == label).count();
    json!({
        "total": all.len(),
        "good": n(Label::Good),
        "bad": n(Label::Bad),
        "no_tools": n(Label::NoTools),
        "needs_tools": n(Label::NeedsTools),
        "corrected": n(Label::Corrected),
        "training": training_lines(all).len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(question: &str, label: Label, tools: &[&str], correction: Option<&str>, at: i64) -> Verdict {
        Verdict::new(question, "an answer", label, tools.iter().map(|t| t.to_string()).collect(), correction, at).unwrap()
    }

    #[test]
    fn words_keep_the_meaning_and_drop_small_words() {
        let w: Vec<String> = words("What is the Capital of France?").into_iter().collect();
        assert_eq!(w, vec!["capital".to_string(), "france".to_string()]);
    }

    #[test]
    fn similarity_is_the_shared_share_of_meaningful_words() {
        assert_eq!(similarity("capital of France", "the capital of france"), 1.0);
        assert_eq!(similarity("hello", "bye"), 0.0);
        assert_eq!(similarity("", "hello"), 0.0);
        // capital and france in common; paris is the third word
        let s = similarity("capital of france", "capital of france paris");
        assert!((s - 2.0 / 3.0).abs() < 1e-9, "{s}");
    }

    #[test]
    fn a_routing_verdict_applies_to_a_similar_question_and_a_correction_never_does() {
        let all = vec![
            v("what time is it", Label::NoTools, &[], None, 1),
            v("what time is it", Label::Corrected, &[], Some("noon"), 5),
        ];
        assert_eq!(taught("what time is it", &all).expect("a routing verdict").label, Label::NoTools);
        assert!(taught("who wrote hamlet", &all).is_none());
    }

    #[test]
    fn among_equal_matches_the_newest_routing_verdict_applies() {
        let all = vec![
            v("list my files", Label::NeedsTools, &["fs_list"], None, 1),
            v("list my files", Label::NoTools, &[], None, 9),
        ];
        assert_eq!(taught("list my files", &all).unwrap().label, Label::NoTools);
    }

    #[test]
    fn a_good_or_bad_verdict_says_nothing_about_tools_and_routes_nothing() {
        let all = vec![
            v("what time is it", Label::Good, &[], None, 1),
            v("what time is it", Label::Bad, &[], None, 2),
        ];
        assert!(taught("what time is it", &all).is_none());
    }

    #[test]
    fn corrections_are_given_most_similar_first_and_at_most_the_limit() {
        let blank = Verdict {
            question: "capital of france".into(),
            answer: "an answer".into(),
            label: Label::Corrected,
            tools: vec![],
            correction: Some("   ".into()),
            at: 3,
        };
        let all = vec![
            v("capital of france", Label::Corrected, &[], Some("Paris"), 1),
            v("capital of france paris", Label::Corrected, &[], Some("It is Paris."), 2),
            blank,
            v("capital of france", Label::Good, &[], None, 4),
        ];
        let found = corrections("capital of france", &all, 2);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].correction.as_deref(), Some("Paris"));
        assert_eq!(found[1].correction.as_deref(), Some("It is Paris."));
    }

    #[test]
    fn a_verdict_needs_a_question_and_a_correction_when_it_corrects() {
        assert!(Verdict::new("  ", "a", Label::Good, vec![], None, 0).is_err());
        assert!(Verdict::new("q", "a", Label::Corrected, vec![], Some("  "), 0).is_err());
        let good = Verdict::new("q", "a", Label::Good, vec![], Some("ignored"), 0).unwrap();
        assert_eq!(good.correction, None);
        let fixed = Verdict::new("q", "a", Label::Corrected, vec![], Some(" right "), 0).unwrap();
        assert_eq!(fixed.correction.as_deref(), Some("right"));
    }

    #[test]
    fn a_long_reply_is_kept_cut_to_its_limit() {
        let long = "x".repeat(MAX_ANSWER_CHARS + 500);
        let kept = Verdict::new("q", &long, Label::Bad, vec![], None, 0).unwrap();
        assert_eq!(kept.answer.chars().count(), MAX_ANSWER_CHARS);
    }

    #[test]
    fn training_lines_are_the_good_replies_and_the_corrections_only() {
        let all = vec![
            v("hello", Label::Good, &[], None, 1),
            v("hello", Label::Bad, &[], None, 2),
            v("what time is it", Label::NoTools, &[], None, 3),
            v("capital of france", Label::Corrected, &[], Some("Paris"), 4),
        ];
        let lines = training_lines(&all);
        assert_eq!(lines.len(), 2);
        let good: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(good["messages"][1]["content"], "an answer");
        assert!(good["metadata"].get("rejected").is_none());
        let fixed: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(fixed["messages"][1]["content"], "Paris");
        assert_eq!(fixed["metadata"]["rejected"], "an answer");
    }

    #[test]
    fn the_correction_note_names_the_question_the_wrong_reply_and_the_right_one() {
        let all = vec![v("capital of france", Label::Corrected, &[], Some("Paris"), 1)];
        let note = correction_note("capital of france", &all).unwrap();
        assert!(note.contains("Asked before: capital of france"), "{note}");
        assert!(note.contains("Correct answer: Paris"), "{note}");
        assert!(correction_note("weather", &all).is_none());
    }

    #[test]
    fn labels_have_stable_wire_names() {
        for label in [Label::Good, Label::Bad, Label::NoTools, Label::NeedsTools, Label::Corrected] {
            assert_eq!(Label::of(label.wire()), Some(label));
        }
        assert_eq!(Label::of("nope"), None);
    }

    #[test]
    fn counts_say_how_many_of_each_kind_and_how_many_train() {
        let all = vec![
            v("a question", Label::Good, &[], None, 1),
            v("a question", Label::Corrected, &[], Some("x"), 2),
        ];
        let c = counts(&all);
        assert_eq!(c["total"], 2);
        assert_eq!(c["corrected"], 1);
        assert_eq!(c["training"], 2);
    }
}

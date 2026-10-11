//! A factory task as the owner or an agent describes it (`km-factory.md` section 2), read and checked once, when
//! the task is created. The stored JSON is the task's source of truth; this is how the steps read it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::Device;
use super::util::slug;
use crate::engine::Fail;

pub const KINDS: [&str; 5] = ["data", "memory", "skill", "tool", "adapter"];
pub const SOURCE_KINDS: [&str; 6] = ["dir", "file", "url", "km", "skill", "tool"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Size {
    /// How many records to generate.
    pub records: u32,
    /// Stop when the model calls have used this many tokens (prompt + completion); 0 = no limit.
    pub max_tokens: u64,
}

impl Default for Size {
    fn default() -> Self {
        Self {
            records: 50,
            max_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tagging {
    /// Which vocabulary to constrain tags to (`default` = the hub's).
    pub vocabulary: String,
    /// `auto` (clean records are approved, doubtful ones and uncertain tags are reviewed), `all` (everything is
    /// reviewed) or `none` (nothing is queued).
    pub review: String,
}

impl Default for Tagging {
    fn default() -> Self {
        Self {
            vocabulary: "default".into(),
            review: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    /// Wall-clock seconds the task may run (summed over runs); 0 = no limit.
    pub wall_s: u64,
    /// A hard cap on generated records (default: size.records).
    pub max_records: u32,
    /// Recorded for the Forge's training budget; the factory itself does not train.
    pub gpu_hours: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Devices {
    pub author: String,
    pub reviewer: String,
    pub base: String,
    pub embedder: String,
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            author: "auto".into(),
            reviewer: "auto".into(),
            base: "auto".into(),
            embedder: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskSpec {
    pub goal: String,
    /// Names the KMs (`data.<name>`...); default: the domain, else the goal's first words.
    pub name: Option<String>,
    pub kinds: Vec<String>,
    pub domain: String,
    pub sources: Vec<Value>,
    pub author: String,
    /// Default: the author (a different model checks better).
    pub reviewer: Option<String>,
    /// The core an adapter is for, and the model the evaluation answers with (default: the author).
    pub base: Option<String>,
    pub size: Size,
    pub tagging: Tagging,
    /// Promotion policy: gated, propose, auto or off (default: the factory's setting).
    pub approval: Option<String>,
    pub budget: Budget,
    pub device: Devices,
    /// Stop after planning until the plan is approved (`factory.task.plan.edit` with `approve`).
    pub plan_approval: bool,
    /// Evaluation sets whose questions must not appear in the records: `{path}` (JSONL), `{ids: [...]}`,
    /// `{hashes: [...]}` (normalised-text hashes).
    pub eval_sets: Vec<Value>,
    pub license: Option<String>,
    /// Let reasoning models think (slower, spends tokens).
    pub think: bool,
    /// Records asked for per model call.
    pub per_call: u32,
}

impl Default for TaskSpec {
    fn default() -> Self {
        Self {
            goal: String::new(),
            name: None,
            kinds: vec!["data".into(), "memory".into()],
            domain: String::new(),
            sources: vec![],
            author: String::new(),
            reviewer: None,
            base: None,
            size: Size::default(),
            tagging: Tagging::default(),
            approval: None,
            budget: Budget::default(),
            device: Devices::default(),
            plan_approval: false,
            eval_sets: vec![],
            license: None,
            think: false,
            per_call: 4,
        }
    }
}

impl TaskSpec {
    pub fn from_json(v: &Value) -> Result<TaskSpec, Fail> {
        let mut s: TaskSpec = serde_json::from_value(v.clone()).map_err(|e| Fail::invalid(format!("task: {e}")))?;
        s.goal = s.goal.trim().to_string();
        if s.goal.is_empty() {
            return Err(Fail::invalid("goal is required: say what the Knowledge Module is for"));
        }
        if s.author.trim().is_empty() {
            return Err(Fail::invalid(
                "author is required: a hub model id (model.list shows them)",
            ));
        }
        if s.kinds.is_empty() {
            return Err(Fail::invalid(
                "kinds must name at least one of data, memory, skill, tool, adapter",
            ));
        }
        for k in &s.kinds {
            if !KINDS.contains(&k.as_str()) {
                return Err(Fail::invalid(format!(
                    "kind {k:?} is not one of {} (the factory builds only these)",
                    KINDS.join(", ")
                )));
            }
        }
        s.kinds.dedup();
        for (what, d) in [
            ("author", &s.device.author),
            ("reviewer", &s.device.reviewer),
            ("base", &s.device.base),
            ("embedder", &s.device.embedder),
        ] {
            Device::parse(d).map_err(|e| Fail::invalid(format!("device.{what}: {e}")))?;
        }
        if !matches!(s.tagging.review.as_str(), "auto" | "all" | "none") {
            return Err(Fail::invalid("tagging.review must be auto, all or none"));
        }
        if s.tagging.vocabulary != "default" {
            return Err(Fail::invalid("tagging.vocabulary: only the default vocabulary exists"));
        }
        if let Some(a) = &s.approval {
            if !matches!(a.as_str(), "gated" | "propose" | "auto" | "off") {
                return Err(Fail::invalid("approval must be gated, propose, auto or off"));
            }
        }
        if s.size.records == 0 {
            return Err(Fail::invalid("size.records must be at least 1"));
        }
        s.per_call = s.per_call.clamp(1, 12);
        for (i, src) in s.sources.iter().enumerate() {
            let kind = src["kind"].as_str().unwrap_or("");
            if !SOURCE_KINDS.contains(&kind) && kind != "traces" {
                return Err(Fail::invalid(format!(
                    "sources[{i}].kind {kind:?} must be one of dir, file, url, km, skill, tool, traces"
                )));
            }
            let need = |k: &str| -> Result<(), Fail> {
                src[k]
                    .as_str()
                    .filter(|x| !x.trim().is_empty())
                    .map(|_| ())
                    .ok_or_else(|| Fail::invalid(format!("sources[{i}] ({kind}) needs {k}")))
            };
            match kind {
                "dir" | "file" | "skill" => need("path")?,
                "url" => {
                    need("url")?;
                    let u = src["url"].as_str().unwrap_or("");
                    if !(u.starts_with("http://") || u.starts_with("https://")) {
                        return Err(Fail::invalid(format!("sources[{i}].url must be http or https")));
                    }
                }
                "km" => need("id")?,
                "tool" => need("name")?,
                "traces" => need("query")?,
                _ => {}
            }
        }
        if s.kinds.iter().any(|k| k == "skill") && !s.sources.iter().any(|x| x["kind"] == "skill") {
            return Err(Fail::invalid(
                "a skill KM needs a source of kind skill (a folder with SKILL.md)",
            ));
        }
        if s.kinds.iter().any(|k| k == "tool") && !s.sources.iter().any(|x| x["kind"] == "tool") {
            return Err(Fail::invalid("a tool KM needs a source of kind tool"));
        }
        if s.sources.is_empty() && s.kinds.iter().any(|k| k == "memory") {
            return Err(Fail::invalid("a memory KM needs at least one source"));
        }
        if s.name_slug().is_empty() {
            return Err(Fail::invalid("name, domain or goal must contain letters or digits"));
        }
        Ok(s)
    }

    /// The part of the KM ids that names this task.
    pub fn name_slug(&self) -> String {
        let from = self
            .name
            .clone()
            .filter(|n| !slug(n).is_empty())
            .or_else(|| Some(self.domain.clone()).filter(|d| !slug(d).is_empty()))
            .unwrap_or_else(|| self.goal.split_whitespace().take(4).collect::<Vec<_>>().join(" "));
        slug(&from)
    }

    pub fn reviewer_model(&self) -> String {
        self.reviewer
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| self.author.clone())
    }

    pub fn base_model(&self) -> String {
        self.base
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| self.author.clone())
    }

    pub fn wants(&self, kind: &str) -> bool {
        self.kinds.iter().any(|k| k == kind)
    }

    pub fn target_records(&self) -> u32 {
        match self.budget.max_records {
            0 => self.size.records,
            m => self.size.records.min(m),
        }
    }

    pub fn dev(&self, which: &str) -> Device {
        let s = match which {
            "author" => &self.device.author,
            "reviewer" => &self.device.reviewer,
            "base" => &self.device.base,
            _ => &self.device.embedder,
        };
        Device::parse(s).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok() -> Value {
        json!({"goal": "Async Rust", "author": "m", "sources": [{"kind": "file", "path": "a.md"}]})
    }

    #[test]
    fn defaults_and_names() {
        let s = TaskSpec::from_json(&ok()).unwrap();
        assert_eq!(s.kinds, ["data", "memory"]);
        assert_eq!(s.reviewer_model(), "m");
        assert_eq!(s.name_slug(), "async-rust");
        let s = TaskSpec::from_json(&json!({"goal": "g", "author": "m", "domain": "code/rust/async",
                                            "sources": [{"kind": "file", "path": "a"}]}))
        .unwrap();
        assert_eq!(s.name_slug(), "code.rust.async");
    }

    #[test]
    fn mistakes_are_refused_with_a_reason() {
        let bad = |mut v: Value, f: &dyn Fn(&mut Value), what: &str| {
            f(&mut v);
            let e = TaskSpec::from_json(&v).unwrap_err().message;
            assert!(e.contains(what), "{what} in {e}");
        };
        bad(ok(), &|v| v["goal"] = json!(" "), "goal");
        bad(ok(), &|v| v["author"] = json!(""), "author");
        bad(ok(), &|v| v["kinds"] = json!(["data", "weights"]), "weights");
        bad(ok(), &|v| v["device"] = json!({"author": "tpu"}), "device.author");
        bad(ok(), &|v| v["sources"] = json!([{"kind": "ftp"}]), "ftp");
        bad(
            ok(),
            &|v| v["sources"] = json!([{"kind": "url", "url": "file:///etc/passwd"}]),
            "http",
        );
        bad(ok(), &|v| v["sources"] = json!([{"kind": "dir"}]), "path");
        bad(ok(), &|v| v["kinds"] = json!(["skill"]), "skill");
        bad(ok(), &|v| v["approval"] = json!("whenever"), "approval");
        bad(ok(), &|v| v["tagging"] = json!({"review": "sometimes"}), "review");
        bad(ok(), &|v| v["sources"] = json!([]), "source");
    }

    #[test]
    fn the_record_cap_wins_over_the_size() {
        let mut v = ok();
        v["size"] = json!({"records": 100});
        v["budget"] = json!({"max_records": 30});
        assert_eq!(TaskSpec::from_json(&v).unwrap().target_records(), 30);
    }
}

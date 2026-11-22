//! Misty's settings: `<home>/misty/config.toml`. Every field is optional; a missing file is the defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What happens when a tool of some class is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Run it.
    Auto,
    /// Pause the run until the owner says yes.
    Ask,
    /// Refuse it.
    Deny,
}

/// The tool classes (see `toolbox`): the policy for each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicySet {
    pub read: Policy,
    pub write: Policy,
    pub exec: Policy,
    pub net: Policy,
    pub hub: Policy,
}

impl Default for PolicySet {
    fn default() -> Self {
        Self {
            read: Policy::Auto,
            write: Policy::Ask,
            exec: Policy::Deny,
            net: Policy::Deny,
            hub: Policy::Ask,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folders the file tools may touch (default: `<home>/misty/workspace`). Nothing outside them, ever.
    pub roots: Vec<PathBuf>,
    pub policy: PolicySet,
    /// Model calls plus tool rounds in one run.
    pub max_steps: u32,
    /// Output tokens per model call.
    pub max_tokens: u32,
    pub run_timeout_s: u64,
    /// How long a run waits for an answer to an approval before it counts as no.
    pub approval_timeout_s: u64,
    /// Retry one role up when an answer looks weak (once per run).
    pub escalate: bool,
    /// Allow the proposers-and-aggregator panel for hard requests (slow on a CPU).
    pub panel: bool,
    /// Let Needle answer a clear tool request without asking a language model.
    pub reflex: bool,
    /// Needle's confidence needed for that shortcut. Measured on the real Needle3 with Misty's tools: a right call
    /// scores about 0.1 to 0.75 and the engine itself returns no call under 0.1, so 0.1 means "a call the engine stands
    /// behind"; a threshold near 1 would never fire. Raise it to be stricter. Only tools that read ever run this way.
    pub reflex_threshold: f32,
    /// Roles whose models are asked to think before answering (the rest answer directly: thinking costs thousands of
    /// tokens on a CPU).
    pub think_roles: Vec<String>,
    pub rag_top_k: usize,
    /// Tokens of retrieved text put into a prompt.
    pub rag_budget_tokens: usize,
    /// The RAG collection used when a request names none.
    pub collection: String,
    /// Role name to the model id that fills it (overrides what setup found).
    pub roles: BTreeMap<String, String>,
    /// Replaces Misty's built-in system prompt.
    pub persona: Option<String>,
    /// CPU threads for Cactus (0: the cores the process may use).
    pub threads: u32,
    /// Free RAM that must stay free after a model loads (MiB).
    pub ram_margin_mib: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![],
            policy: PolicySet::default(),
            max_steps: 8,
            max_tokens: 2048,
            run_timeout_s: 900,
            approval_timeout_s: 300,
            escalate: true,
            panel: false,
            reflex: true,
            reflex_threshold: 0.10,
            think_roles: vec!["deep".into()],
            rag_top_k: 6,
            rag_budget_tokens: 1500,
            collection: "default".into(),
            roles: BTreeMap::new(),
            persona: None,
            threads: 0,
            ram_margin_mib: 2048,
        }
    }
}

impl Config {
    pub fn path(misty_dir: &Path) -> PathBuf {
        misty_dir.join("config.toml")
    }

    /// The file, or the defaults when there is none. A file that does not parse is an error, not a silent reset.
    pub fn load(misty_dir: &Path) -> Result<Self, String> {
        let p = Self::path(misty_dir);
        match std::fs::read_to_string(&p) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    pub fn save(&self, misty_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(misty_dir).map_err(|e| e.to_string())?;
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        let p = Self::path(misty_dir);
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// The roots with the default workspace filled in.
    pub fn roots_or_default(&self, misty_dir: &Path) -> Vec<PathBuf> {
        if self.roots.is_empty() {
            vec![misty_dir.join("workspace")]
        } else {
            self.roots.clone()
        }
    }

    pub fn thinks(&self, role: &str) -> bool {
        self.think_roles.iter().any(|r| r == role)
    }

    /// Checks values a user can get wrong.
    pub fn check(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.reflex_threshold) {
            return Err("reflex_threshold must be between 0 and 1".into());
        }
        if self.max_steps == 0 || self.max_steps > 64 {
            return Err("max_steps must be 1 to 64".into());
        }
        if self.rag_top_k == 0 || self.rag_top_k > 50 {
            return Err("rag_top_k must be 1 to 50".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults_and_a_bad_one_is_an_error() {
        let d = crate::testing::TestDir::new("misty-cfg");
        assert_eq!(Config::load(&d).unwrap(), Config::default());
        std::fs::write(Config::path(&d), "max_steps = \"many\"").unwrap();
        assert!(Config::load(&d).unwrap_err().contains("config.toml"));
    }

    #[test]
    fn saved_settings_come_back_and_exec_is_denied_by_default() {
        let d = crate::testing::TestDir::new("misty-cfg2");
        let mut c = Config {
            max_steps: 3,
            ..Default::default()
        };
        c.policy.write = Policy::Auto;
        c.roles.insert("main".into(), "my-model".into());
        c.save(&d).unwrap();
        let back = Config::load(&d).unwrap();
        assert_eq!(back, c);
        assert_eq!(Config::default().policy.exec, Policy::Deny);
        assert_eq!(Config::default().policy.net, Policy::Deny);
        assert!(back.thinks("deep") && !back.thinks("main"));
    }

    #[test]
    fn check_refuses_out_of_range_values() {
        let mut c = Config {
            reflex_threshold: 1.5,
            ..Default::default()
        };
        assert!(c.check().is_err());
        c.reflex_threshold = 0.5;
        c.max_steps = 0;
        assert!(c.check().is_err());
        c.max_steps = 4;
        assert!(c.check().is_ok());
    }
}

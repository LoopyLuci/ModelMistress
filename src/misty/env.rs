//! Environments and working folders: where a Misty session works and what it may do there.
//!
//! An environment is a named, reusable setup the owner makes once: a working folder, more folders the file tools may
//! reach, environment variables for `shell_run`, and a policy for each class of tool that replaces Misty's own for
//! sessions in it. A session picks an environment and may name a folder of its own, which wins over the environment's.
//!
//! Everything here is the owner's choice and only the owner's token can make it (`misty.env.*`, `misty.session.set`):
//! a model has no tool that reaches these, so it cannot widen its own folders or loosen its own policy. The guards in
//! `toolbox` (no secrets, nothing outside the folders) hold inside whatever folders are chosen.
//!
//! Without an environment and without a folder a session works as before: the folders and policy in Misty's settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::config::{Config, Policy, PolicySet};
use super::toolbox::{self, Class};

const MAX_NAME: usize = 60;
const MAX_NOTE: usize = 500;
const MAX_ROOTS: usize = 16;
const MAX_VARS: usize = 64;
const MAX_VALUE: usize = 4096;
/// Entries one folder listing returns (a folder with more says so).
const MAX_LISTED: usize = 1000;

/// A policy for each class of tool; a class left out keeps Misty's own setting.
/// A class name that is not one (a typo for `exec`) is an error: a silently ignored policy is the wrong kind of surprise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<Policy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<Policy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<Policy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net: Option<Policy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<Policy>,
    /// A policy for single tools, which wins over any class setting for them.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Policy>,
}

impl PolicyOverride {
    fn of_class(&self, c: Class) -> Option<Policy> {
        match c {
            Class::Read => self.read,
            Class::Write => self.write,
            Class::Exec => self.exec,
            Class::Net => self.net,
            Class::Hub => self.hub,
        }
    }

    /// `base` with this environment's choices on top. A class the environment sets replaces what Misty's own settings
    /// say about single tools of that class (the environment is the narrower, newer choice, and a per-tool entry in
    /// Misty's settings must not undo an environment that turns a whole class off); the environment's own per-tool
    /// entries then apply.
    pub fn apply(&self, base: &PolicySet) -> PolicySet {
        let mut tools = base.tools.clone();
        tools.retain(|name, _| {
            toolbox::find(name)
                .map(|s| self.of_class(s.class).is_none())
                .unwrap_or(true)
        });
        tools.extend(self.tools.clone());
        PolicySet {
            read: self.read.unwrap_or(base.read),
            write: self.write.unwrap_or(base.write),
            exec: self.exec.unwrap_or(base.exec),
            net: self.net.unwrap_or(base.net),
            hub: self.hub.unwrap_or(base.hub),
            tools,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The working folder: relative paths in the file tools and the shell start here.
    pub cwd: Option<String>,
    /// More folders the file tools may reach.
    pub roots: Vec<String>,
    /// Passed to `shell_run` (which otherwise runs with almost nothing). Stored as written: do not put anything here
    /// that the model must not be able to print.
    pub vars: BTreeMap<String, String>,
    pub policy: PolicyOverride,
    pub created: i64,
    pub updated: i64,
}

/// A path without Windows' `\\?\` prefix (what `canonicalize` returns), which nobody wants to read.
pub fn plain(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

/// Checks that `path` is the full path of a folder that exists, and returns it tidied (no trailing separator).
pub fn check_folder(path: &str) -> Result<String, String> {
    let t = path.trim().trim_matches('"');
    if t.is_empty() {
        return Err("a folder is needed".into());
    }
    let p = Path::new(t);
    if !p.is_absolute() {
        return Err(format!(
            "{t}: give the full path of the folder (for example {})",
            example_path()
        ));
    }
    let md = std::fs::metadata(p).map_err(|e| format!("{t}: {}", reason(&e)))?;
    if !md.is_dir() {
        return Err(format!("{t}: that is a file, not a folder"));
    }
    let tidy = t.trim_end_matches(['/', '\\']);
    // `C:\` and `/` are folders; trimming them would leave `C:` (the current folder of a drive) or nothing
    if tidy.is_empty() || tidy.ends_with(':') {
        return Ok(t.to_string());
    }
    Ok(tidy.to_string())
}

fn example_path() -> &'static str {
    if cfg!(windows) {
        r"C:\Projects\app"
    } else {
        "/home/you/app"
    }
}

fn reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "there is no such folder".into(),
        std::io::ErrorKind::PermissionDenied => "this program may not open that folder".into(),
        _ => e.to_string(),
    }
}

fn valid_var_name(k: &str) -> bool {
    let mut c = k.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
        && k.len() <= 128
}

impl Environment {
    /// Tidies the fields and refuses what cannot work: no name, a folder that is not there, a variable name the
    /// shell would not accept. The folders are checked now, and again by the tools each time they are used.
    pub fn check(&mut self) -> Result<(), String> {
        self.name = self.name.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.name.is_empty() {
            return Err("an environment needs a name".into());
        }
        if self.name.chars().count() > MAX_NAME {
            return Err(format!("the name is longer than {MAX_NAME} characters"));
        }
        self.description = self.description.trim().to_string();
        if self.description.chars().count() > MAX_NOTE {
            return Err(format!("the description is longer than {MAX_NOTE} characters"));
        }
        self.cwd = match self.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(check_folder(c).map_err(|e| format!("working folder: {e}"))?),
            None => None,
        };
        if self.roots.len() > MAX_ROOTS {
            return Err(format!("at most {MAX_ROOTS} extra folders"));
        }
        let mut roots: Vec<String> = vec![];
        for r in std::mem::take(&mut self.roots) {
            if r.trim().is_empty() {
                continue;
            }
            let r = check_folder(&r).map_err(|e| format!("extra folder: {e}"))?;
            if self.cwd.as_deref() != Some(r.as_str()) && !roots.contains(&r) {
                roots.push(r);
            }
        }
        self.roots = roots;
        for name in self.policy.tools.keys() {
            if toolbox::find(name).is_none() {
                return Err(format!("policy.tools: there is no tool named {name}"));
            }
        }
        if self.vars.len() > MAX_VARS {
            return Err(format!("at most {MAX_VARS} variables"));
        }
        for (k, v) in &self.vars {
            if !valid_var_name(k) {
                return Err(format!(
                    "variable {k:?}: a name is letters, digits and underscores, and does not start with a digit"
                ));
            }
            if v.len() > MAX_VALUE || v.contains('\0') {
                return Err(format!("variable {k}: the value is too long, or has a NUL character"));
            }
        }
        Ok(())
    }
}

/// What one run may do and where: the environment and folder of its session resolved against Misty's settings.
/// Built fresh for every run and every tool call, so a change to the environment shows from the next call on.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    /// The environment's name, when the session has one.
    pub env: Option<String>,
    /// The working folder, when the session (or its environment) chose one. `None`: Misty's own workspace.
    pub cwd: Option<PathBuf>,
    /// Folders the file tools may touch; a relative path is under the first.
    pub roots: Vec<PathBuf>,
    /// Extra variables for `shell_run`.
    pub vars: BTreeMap<String, String>,
    pub policy: PolicySet,
}

impl Scope {
    /// `cwd` is the session's own folder; it wins over the environment's.
    pub fn of(cfg: &Config, misty_dir: &Path, env: Option<&Environment>, cwd: Option<&str>) -> Self {
        let chosen = cwd
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .or_else(|| env.and_then(|e| e.cwd.as_deref()))
            .map(PathBuf::from);
        let mut roots: Vec<PathBuf> = chosen.iter().cloned().collect();
        for r in env.map(|e| e.roots.as_slice()).unwrap_or_default() {
            let r = PathBuf::from(r);
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        let has_own_folders = !roots.is_empty();
        if !has_own_folders {
            roots = cfg.roots_or_default(misty_dir);
        }
        Scope {
            env: env.map(|e| e.name.clone()),
            cwd: if has_own_folders { roots.first().cloned() } else { None },
            roots,
            vars: env.map(|e| e.vars.clone()).unwrap_or_default(),
            policy: env
                .map(|e| e.policy.apply(&cfg.policy))
                .unwrap_or_else(|| cfg.policy.clone()),
        }
    }

    /// `cfg` with this scope's policy and folders in it, for the parts of a run that read the settings.
    pub fn applied(&self, mut cfg: Config) -> Config {
        cfg.policy = self.policy.clone();
        cfg.roots = self.roots.clone();
        cfg
    }

    /// What the model is told about where it is working, or `None` for Misty's own workspace (as before).
    pub fn prompt(&self) -> Option<String> {
        let cwd = self.cwd.as_ref()?;
        let mut s = format!(
            "# Working folder\nYou are working in {}. A path without a folder is inside it.",
            plain(cwd)
        );
        if self.roots.len() > 1 {
            let more: Vec<String> = self.roots[1..].iter().map(|r| plain(r)).collect();
            s.push_str(&format!(" You may also use these folders: {}.", more.join(", ")));
        }
        Some(s)
    }

    /// The scope as the owner's screens show it.
    pub fn describe(&self) -> Value {
        json!({"environment": self.env, "cwd": self.cwd.as_deref().map(plain),
               "roots": self.roots.iter().map(|r| plain(r)).collect::<Vec<_>>(), "policy": self.policy})
    }
}

// ---- folder browser --------------------------------------------------------------------------------------------------

/// The usual starting places: the home folder and its well-known folders, then the drives.
fn places() -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let mut add = |label: &str, p: PathBuf| {
        if p.is_dir() && !out.iter().any(|o| o["path"] == plain(&p)) {
            out.push(json!({"label": label, "path": plain(&p)}));
        }
    };
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from) {
        for (label, sub) in [
            ("Documents", "Documents"),
            ("Desktop", "Desktop"),
            ("Downloads", "Downloads"),
        ] {
            add(label, home.join(sub));
        }
        add("Home", home);
    }
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut mounts: Vec<PathBuf> = disks.iter().map(|d| d.mount_point().to_path_buf()).collect();
    mounts.sort();
    mounts.dedup();
    for m in mounts {
        let label = plain(&m);
        add(&label, m);
    }
    // Home first: it is where most people want to start
    out.sort_by_key(|o| !(o["label"] == "Home"));
    out
}

/// The folders inside `path`, for choosing a working folder. With no path: the starting places. Lists names only,
/// and only folders: no file is opened or described.
pub fn browse(path: Option<&str>, hidden: bool) -> Result<Value, String> {
    let Some(t) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(json!({"path": null, "parent": null, "dirs": [], "places": places(), "truncated": false}));
    };
    // Rebuilt from its parts so one separator is used all through: a path typed or saved with `/` on Windows would
    // otherwise list its folders as `D:/work\docs`.
    let p: PathBuf = Path::new(&check_folder(t)?).components().collect();
    let shown = plain(&p);
    let p = p.as_path();
    let mut dirs: Vec<(String, String)> = vec![];
    let mut truncated = false;
    for e in std::fs::read_dir(p)
        .map_err(|e| format!("{shown}: {}", reason(&e)))?
        .flatten()
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        // a link to a folder counts as a folder
        if !e.path().is_dir() {
            continue;
        }
        if dirs.len() >= MAX_LISTED {
            truncated = true;
            break;
        }
        dirs.push((name, plain(&e.path())));
    }
    dirs.sort_by_key(|(n, _)| n.to_lowercase());
    Ok(json!({
        "path": shown,
        "parent": p.parent().filter(|x| !x.as_os_str().is_empty()).map(plain),
        "dirs": dirs.into_iter().map(|(name, path)| json!({"name": name, "path": path})).collect::<Vec<_>>(),
        "places": places(),
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDir;

    fn env_in(d: &Path) -> Environment {
        std::fs::create_dir_all(d.join("proj")).unwrap();
        Environment {
            name: "  Dev   box ".into(),
            cwd: Some(d.join("proj").to_string_lossy().into_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn an_environment_is_tidied_and_what_cannot_work_is_refused() {
        let d = TestDir::new("env-check");
        let mut e = env_in(&d);
        e.vars.insert("API_URL".into(), "http://x".into());
        e.check().unwrap();
        assert_eq!(e.name, "Dev box");
        assert!(e.cwd.as_deref().unwrap().ends_with("proj"));

        let mut bad = env_in(&d);
        bad.name = "  ".into();
        assert!(bad.check().unwrap_err().contains("needs a name"));

        let mut bad = env_in(&d);
        bad.cwd = Some(d.join("nowhere").to_string_lossy().into_owned());
        let err = bad.check().unwrap_err();
        assert!(
            err.contains("working folder") && err.contains("no such folder"),
            "{err}"
        );

        let mut bad = env_in(&d);
        bad.cwd = Some("relative/dir".into());
        assert!(bad.check().unwrap_err().contains("full path"));

        let mut bad = env_in(&d);
        std::fs::write(d.join("file.txt"), "x").unwrap();
        bad.roots = vec![d.join("file.txt").to_string_lossy().into_owned()];
        assert!(bad.check().unwrap_err().contains("not a folder"));

        for name in ["1ST", "A-B", "A B", "", "A=B"] {
            let mut bad = env_in(&d);
            bad.vars.insert(name.into(), "v".into());
            assert!(bad.check().unwrap_err().contains("variable"), "{name:?}");
        }
        let mut bad = env_in(&d);
        bad.vars.insert("K".into(), "a\0b".into());
        assert!(bad.check().is_err());
    }

    #[test]
    fn extra_folders_drop_duplicates_and_the_working_folder_itself() {
        let d = TestDir::new("env-roots");
        let mut e = env_in(&d);
        std::fs::create_dir_all(d.join("data")).unwrap();
        let cwd = e.cwd.clone().unwrap();
        let data = d.join("data").to_string_lossy().into_owned();
        e.roots = vec![cwd, data.clone(), format!("{data}/"), "  ".into()];
        e.check().unwrap();
        assert_eq!(e.roots, vec![data]);
    }

    #[test]
    fn a_session_without_an_environment_or_folder_keeps_mistys_own_settings() {
        let d = TestDir::new("env-base");
        let cfg = Config::default();
        let s = Scope::of(&cfg, &d, None, None);
        assert!(s.env.is_none() && s.cwd.is_none());
        assert_eq!(s.roots, vec![d.join("workspace")]);
        assert_eq!(s.policy, cfg.policy);
        assert!(s.vars.is_empty() && s.prompt().is_none());
    }

    #[test]
    fn the_session_folder_beats_the_environments_and_the_policy_is_overridden_per_class() {
        let d = TestDir::new("env-scope");
        let mut e = env_in(&d);
        e.roots = vec![d.join("data").to_string_lossy().into_owned()];
        e.policy.exec = Some(Policy::Ask);
        e.vars.insert("MODE".into(), "dev".into());
        let cfg = Config::default();

        let s = Scope::of(&cfg, &d, Some(&e), None);
        assert_eq!(s.env.as_deref(), Some(e.name.as_str()));
        assert_eq!(s.roots, vec![PathBuf::from(e.cwd.clone().unwrap()), d.join("data")]);
        assert_eq!(s.cwd.as_deref(), Some(Path::new(e.cwd.as_deref().unwrap())));
        // only exec changed
        assert_eq!(
            (s.policy.exec, s.policy.read, s.policy.write, s.policy.net),
            (Policy::Ask, cfg.policy.read, cfg.policy.write, cfg.policy.net)
        );
        assert_eq!(s.vars["MODE"], "dev");

        // the session's own folder is the first root and the environment's folder is not kept
        let own = d.join("other").to_string_lossy().into_owned();
        let s = Scope::of(&cfg, &d, Some(&e), Some(&own));
        assert_eq!(s.roots[0], PathBuf::from(&own));
        assert_eq!(s.cwd.as_deref(), Some(Path::new(&own)));
        assert!(s.roots.contains(&d.join("data")));

        // a folder alone, with no environment
        let s = Scope::of(&cfg, &d, None, Some(&own));
        assert_eq!(s.roots, vec![PathBuf::from(&own)]);
        assert!(s.env.is_none());

        // the settings a run reads carry the scope
        let applied = s.applied(Config::default());
        assert_eq!(applied.roots, vec![PathBuf::from(&own)]);
    }

    #[test]
    fn an_environment_with_no_folders_works_in_mistys_workspace() {
        let d = TestDir::new("env-nofolder");
        let e = Environment {
            name: "plain".into(),
            policy: PolicyOverride {
                write: Some(Policy::Auto),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = Scope::of(&Config::default(), &d, Some(&e), None);
        assert_eq!(s.roots, vec![d.join("workspace")]);
        assert!(s.cwd.is_none() && s.prompt().is_none());
        assert_eq!(s.policy.write, Policy::Auto);
        assert_eq!(s.env.as_deref(), Some("plain"));
    }

    #[test]
    fn the_prompt_names_the_working_folder_and_the_others() {
        let d = TestDir::new("env-prompt");
        let mut e = env_in(&d);
        e.roots = vec![d.join("data").to_string_lossy().into_owned()];
        let p = Scope::of(&Config::default(), &d, Some(&e), None).prompt().unwrap();
        assert!(
            p.contains("# Working folder") && p.contains("proj") && p.contains("also use"),
            "{p}"
        );
        assert!(p.contains("data"), "{p}");
    }

    #[test]
    fn windows_verbatim_prefixes_are_not_shown() {
        assert_eq!(plain(Path::new(r"\\?\C:\Users\me")), r"C:\Users\me");
        assert_eq!(plain(Path::new(r"\\?\UNC\srv\share\x")), r"\\srv\share\x");
        assert_eq!(plain(Path::new("/home/me")), "/home/me");
    }

    #[test]
    fn browsing_lists_folders_only_sorted_and_says_where_to_go_up() {
        let d = TestDir::new("env-browse");
        for n in ["beta", "Alpha", ".hidden", "gamma"] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        std::fs::write(d.join("a-file.txt"), "x").unwrap();
        let v = browse(Some(&d.to_string_lossy()), false).unwrap();
        let names: Vec<&str> = v["dirs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Alpha", "beta", "gamma"]);
        assert!(v["parent"].is_string() && v["truncated"] == false);
        let all = browse(Some(&d.to_string_lossy()), true).unwrap();
        assert_eq!(all["dirs"].as_array().unwrap().len(), 4);
        // each entry's path can be browsed again
        let inner = v["dirs"][0]["path"].as_str().unwrap();
        assert_eq!(browse(Some(inner), false).unwrap()["dirs"].as_array().unwrap().len(), 0);
        // a missing folder and a file are named, not listed
        assert!(browse(Some(&d.join("nope").to_string_lossy()), false)
            .unwrap_err()
            .contains("no such folder"));
        assert!(browse(Some(&d.join("a-file.txt").to_string_lossy()), false)
            .unwrap_err()
            .contains("not a folder"));
        assert!(browse(Some("relative"), false).unwrap_err().contains("full path"));
    }

    #[cfg(windows)]
    #[test]
    fn browsing_a_path_written_with_forward_slashes_shows_one_kind_of_slash() {
        let d = TestDir::new("env-browse-slashes");
        std::fs::create_dir_all(d.join("docs")).unwrap();
        let v = browse(Some(&d.to_string_lossy().replace('\\', "/")), false).unwrap();
        assert!(!v["path"].as_str().unwrap().contains('/'), "{v}");
        assert!(!v["dirs"][0]["path"].as_str().unwrap().contains('/'), "{v}");
    }

    #[test]
    fn a_tool_policy_in_an_environment_wins_and_a_class_it_sets_replaces_mistys_entries_for_that_class() {
        let mut base = PolicySet::default();
        base.tools.insert("fs_write".into(), Policy::Auto); // Misty's own: this one write tool runs freely
        base.tools.insert("fs_read".into(), Policy::Ask);
        // an environment that turns the whole write class off is not undone by Misty's per-tool entry
        let off = PolicyOverride {
            write: Some(Policy::Deny),
            ..Default::default()
        };
        let p = off.apply(&base);
        assert_eq!(toolbox::find("fs_write").unwrap().policy(&p), Policy::Deny);
        assert_eq!(
            toolbox::find("fs_read").unwrap().policy(&p),
            Policy::Ask,
            "another class keeps its entry"
        );
        // its own per-tool entry wins over its class and over Misty's
        let mut own = PolicyOverride {
            write: Some(Policy::Deny),
            ..Default::default()
        };
        own.tools.insert("fs_write".into(), Policy::Ask);
        assert_eq!(
            toolbox::find("fs_write").unwrap().policy(&own.apply(&base)),
            Policy::Ask
        );
        // a name that is not a tool is refused when the environment is saved
        let d = TestDir::new("env-tool-policy");
        let mut e = Environment {
            name: "p".into(),
            cwd: Some(d.to_string_lossy().into_owned()),
            ..Default::default()
        };
        e.policy.tools.insert("fs_wirte".into(), Policy::Auto);
        assert!(e.check().unwrap_err().contains("fs_wirte"));
        e.policy.tools.clear();
        e.policy.tools.insert("fs_write".into(), Policy::Auto);
        assert!(e.check().is_ok());
        // it survives the JSON round trip the operations use, and an unknown field is still refused
        let back: PolicyOverride = serde_json::from_value(serde_json::to_value(&e.policy).unwrap()).unwrap();
        assert_eq!(back, e.policy);
        assert!(serde_json::from_value::<PolicyOverride>(json!({"tool": {"calc": "auto"}})).is_err());
    }

    #[test]
    fn browsing_with_no_path_offers_places_to_start() {
        let v = browse(None, false).unwrap();
        assert!(v["path"].is_null());
        let places = v["places"].as_array().unwrap();
        assert!(!places.is_empty(), "a machine has at least one drive or a home folder");
        for p in places {
            assert!(Path::new(p["path"].as_str().unwrap()).is_dir());
        }
    }
}

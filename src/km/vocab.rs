//! The controlled tag vocabulary (km-factory.md section 5): `<home>/km/vocabulary.toml`.
//!
//! Tags are `namespace:value`. A namespace lists its approved values; an `open` namespace takes any value. New values
//! enter only when approved (`km.vocabulary.edit`). The file is optional: without it the built-in defaults apply. A KM
//! with a tag outside the vocabulary is still installed (the vocabulary guides, it does not block); the import answer
//! and `km.show` list such tags so they can be reviewed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::Res;
use crate::engine::Fail;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Namespace {
    pub description: String,
    /// Any value is accepted.
    pub open: bool,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vocabulary {
    pub namespaces: BTreeMap<String, Namespace>,
}

fn ns(description: &str, open: bool, values: &[&str]) -> Namespace {
    Namespace {
        description: description.into(),
        open,
        values: values.iter().map(|s| s.to_string()).collect(),
    }
}

impl Default for Vocabulary {
    fn default() -> Self {
        let namespaces = BTreeMap::from([
            (
                "task".to_string(),
                ns(
                    "what the KM helps do",
                    false,
                    &[
                        "codegen",
                        "debug",
                        "explain",
                        "summarise",
                        "translate",
                        "plan",
                        "tool-use",
                        "chat",
                        "extract",
                    ],
                ),
            ),
            ("lang".to_string(), ns("natural or programming language", true, &[])),
            ("lib".to_string(), ns("library or framework", true, &[])),
            (
                "modality".to_string(),
                ns("kinds of input and output", false, &["text", "image", "audio", "video"]),
            ),
            ("skill".to_string(), ns("a named skill", true, &[])),
            ("tool".to_string(), ns("a tool the KM concerns", true, &[])),
            ("source".to_string(), ns("where the content came from", true, &[])),
            ("license".to_string(), ns("licence of the content", true, &[])),
            (
                "quality".to_string(),
                ns("how good the content is", false, &["gold", "silver", "synthetic"]),
            ),
            (
                "risk".to_string(),
                ns("what could go wrong with it", false, &["none", "pii", "security"]),
            ),
        ]);
        Self { namespaces }
    }
}

fn valid_part(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| !c.is_whitespace() && !c.is_control() && c != ':')
}

impl Vocabulary {
    pub fn path(km_root: &Path) -> PathBuf {
        km_root.join("vocabulary.toml")
    }

    pub fn load(km_root: &Path) -> Res<Vocabulary> {
        let p = Self::path(km_root);
        match std::fs::read_to_string(&p) {
            Ok(t) => toml::from_str(&t).map_err(|e| Fail::invalid(format!("{}: {e}", p.display()))),
            Err(_) => Ok(Vocabulary::default()),
        }
    }

    pub fn save(&self, km_root: &Path) -> Res<()> {
        let p = Self::path(km_root);
        let text = toml::to_string_pretty(self).map_err(|e| Fail::new(500, "io", e.to_string()))?;
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &p))
            .map_err(|e| Fail::new(500, "io", format!("{}: {e}", p.display())))
    }

    /// Why a tag is outside the vocabulary, or None if it is approved.
    pub fn problem(&self, tag: &str) -> Option<String> {
        let Some((n, v)) = tag.split_once(':') else {
            return Some(format!("{tag:?} is not namespace:value"));
        };
        match self.namespaces.get(n) {
            None => Some(format!("namespace {n:?} is not in the vocabulary")),
            Some(x) if x.open || x.values.iter().any(|a| a == v) => None,
            Some(_) => Some(format!("{v:?} is not an approved value of {n}:")),
        }
    }

    pub fn unapproved(&self, tags: &[String]) -> Vec<Value> {
        tags.iter()
            .filter_map(|t| self.problem(t).map(|p| json!({"tag": t, "problem": p})))
            .collect()
    }

    /// Applies an edit: add/remove values of a namespace, create it, set `open` or its description, or delete it.
    pub fn edit(&mut self, a: &Value) -> Res<()> {
        let name = a["namespace"]
            .as_str()
            .ok_or_else(|| Fail::invalid("namespace is required"))?;
        if !valid_part(name) {
            return Err(Fail::invalid("a namespace is a short word without spaces or ':'"));
        }
        if a["delete"].as_bool().unwrap_or(false) {
            return self
                .namespaces
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| Fail::not_found(format!("no namespace {name}")));
        }
        let list = |k: &str| -> Res<Vec<String>> {
            match a.get(k) {
                None | Some(Value::Null) => Ok(vec![]),
                Some(Value::Array(v)) => v
                    .iter()
                    .map(|x| x.as_str().map(str::to_string).filter(|s| valid_part(s)))
                    .collect::<Option<_>>()
                    .ok_or_else(|| Fail::invalid(format!("{k} must be a list of short words without spaces or ':'"))),
                Some(_) => Err(Fail::invalid(format!("{k} must be a list"))),
            }
        };
        let (add, remove) = (list("add")?, list("remove")?);
        let exists = self.namespaces.contains_key(name);
        if !exists && add.is_empty() && a.get("open").is_none() {
            return Err(Fail::not_found(format!(
                "no namespace {name}; give add (values) or open to create it"
            )));
        }
        let n = self.namespaces.entry(name.to_string()).or_default();
        if let Some(o) = a["open"].as_bool() {
            n.open = o;
        }
        if let Some(d) = a["description"].as_str() {
            n.description = d.to_string();
        }
        for v in remove {
            n.values.retain(|x| *x != v);
        }
        for v in add {
            if !n.values.contains(&v) {
                n.values.push(v);
            }
        }
        n.values.sort();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_the_factory_namespaces() {
        let v = Vocabulary::default();
        for n in [
            "task", "lang", "lib", "modality", "skill", "tool", "source", "license", "quality", "risk",
        ] {
            assert!(v.namespaces.contains_key(n), "{n}");
        }
        assert!(v.problem("task:codegen").is_none());
        assert!(v.problem("lang:rust").is_none(), "lang is open");
        assert!(v.problem("task:dancing").unwrap().contains("not an approved value"));
        assert!(v.problem("colour:red").unwrap().contains("not in the vocabulary"));
        assert!(v.problem("plain").unwrap().contains("namespace:value"));
        assert_eq!(v.unapproved(&["task:codegen".into(), "risk:high".into()]).len(), 1);
    }

    #[test]
    fn edits_approve_new_values_and_persist() {
        let dir = crate::testing::temp_home();
        let mut v = Vocabulary::load(&dir).unwrap();
        assert!(v.problem("task:dancing").is_some());
        v.edit(&json!({"namespace": "task", "add": ["dancing"]})).unwrap();
        v.edit(&json!({"namespace": "colour", "add": ["red"], "description": "paint"}))
            .unwrap();
        v.edit(&json!({"namespace": "risk", "remove": ["pii"]})).unwrap();
        v.edit(&json!({"namespace": "lib", "open": false})).unwrap();
        v.save(&dir).unwrap();
        let back = Vocabulary::load(&dir).unwrap();
        assert_eq!(back, v);
        assert!(back.problem("task:dancing").is_none() && back.problem("colour:red").is_none());
        assert!(back.problem("risk:pii").is_some() && back.problem("lib:tokio").is_some());
        back.clone()
            .edit(&json!({"namespace": "colour", "delete": true}))
            .unwrap();
        assert!(v.edit(&json!({"namespace": "nope"})).is_err());
        assert!(v.edit(&json!({"namespace": "bad name"})).is_err());
        assert!(v.edit(&json!({"namespace": "task", "add": ["a:b"]})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

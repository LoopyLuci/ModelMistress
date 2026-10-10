//! `requires`, `conflicts` and bundles (km-spec.md §5): the set of KMs a load needs, or the reason it cannot be had.
//!
//! The set is built depth first, dependencies before dependents. For each requirement the version already resident
//! is preferred when it satisfies the range, then the `stable` channel's version, then the newest installed version
//! that does. A cycle, a range nothing satisfies, two ranges on the same KM that no one version satisfies, and a
//! conflict between any two members of the set (or with something resident) each refuse the load with the path that
//! led there.

use std::collections::BTreeMap;

use semver::VersionReq;

use super::manifest::parse_ref;
use super::store::{Record, Store};
use super::Res;
use crate::engine::Fail;

fn fail(code: &'static str, message: String) -> Fail {
    Fail::new(409, code, message)
}

struct Walk<'a> {
    store: &'a Store,
    resident: &'a [Record],
    /// id -> chosen record, in load order.
    chosen: BTreeMap<String, Record>,
    order: Vec<String>,
    /// The ranges asked of each id so far, with who asked.
    asked: BTreeMap<String, Vec<(String, VersionReq)>>,
    stack: Vec<String>,
}

impl Walk<'_> {
    fn need(&mut self, by: &Record, spec: &str) -> Res<()> {
        let (id, req) = parse_ref(spec).map_err(|e| fail("bad_requirement", format!("{}: {e}", by.key)))?;
        self.asked
            .entry(id.clone())
            .or_default()
            .push((by.key.clone(), req.clone()));
        if self.stack.iter().any(|k| k.split('@').next() == Some(id.as_str())) {
            let mut path = self.stack.clone();
            path.push(format!("{id}@{req}"));
            return Err(fail("cycle", format!("a dependency cycle: {}", path.join(" -> "))));
        }
        if let Some(have) = self.chosen.get(&id) {
            if !req.matches(&have.version()) {
                return Err(fail(
                    "unsatisfiable",
                    format!(
                        "{} requires {id}@{req}, but {} is already in the set (wanted by {})",
                        by.key,
                        have.key,
                        self.asked[&id]
                            .iter()
                            .filter(|(w, _)| *w != by.key)
                            .map(|(w, r)| format!("{w} wants {r}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            return Ok(());
        }
        let wanted = self.asked[&id].clone();
        let ok = |r: &Record| wanted.iter().all(|(_, q)| q.matches(&r.version()));
        let pick = self
            .resident
            .iter()
            .find(|r| r.id == id && ok(r))
            .cloned()
            .or_else(|| {
                self.store
                    .channel_get(&id, "stable")
                    .and_then(|v| self.store.get(&format!("{id}@{v}")).ok().flatten())
                    .filter(|r| ok(r))
            })
            .or_else(|| {
                self.store
                    .versions(&id)
                    .ok()
                    .and_then(|v| v.into_iter().find(|r| ok(r)))
            });
        match pick {
            Some(r) => self.visit(r),
            None => {
                let have: Vec<String> = self
                    .store
                    .versions(&id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| r.version)
                    .collect();
                Err(fail(
                    "unsatisfiable",
                    format!(
                        "{} requires {id}@{req}, but {}",
                        by.key,
                        if have.is_empty() {
                            format!("{id} is not installed")
                        } else {
                            format!("only {} of {id} is installed", have.join(", "))
                        }
                    ),
                ))
            }
        }
    }

    fn visit(&mut self, r: Record) -> Res<()> {
        if self.chosen.contains_key(&r.id) {
            return Ok(());
        }
        self.stack.push(r.key.clone());
        let mut deps: Vec<String> = r.requires.clone();
        if r.kind == "bundle" {
            let m = super::manifest::Manifest::from_file(&self.store.dir_of(&r.key).join("km.toml"))?;
            deps.extend(m.members);
        }
        for d in deps {
            self.need(&r, &d)?;
        }
        self.stack.pop();
        self.order.push(r.id.clone());
        self.chosen.insert(r.id.clone(), r);
        Ok(())
    }
}

/// The records to load for `roots`, dependencies first (the roots last). `resident` are KMs already loaded.
pub fn closure(store: &Store, roots: &[Record], resident: &[Record]) -> Res<Vec<Record>> {
    let mut w = Walk {
        store,
        resident,
        chosen: BTreeMap::new(),
        order: vec![],
        asked: BTreeMap::new(),
        stack: vec![],
    };
    for r in roots {
        w.visit(r.clone())?;
    }
    let set: Vec<Record> = w.order.iter().filter_map(|id| w.chosen.get(id).cloned()).collect();
    // conflicts: inside the set, and with what is resident
    let others: Vec<&Record> = resident.iter().filter(|r| !set.iter().any(|s| s.id == r.id)).collect();
    let everyone: Vec<&Record> = set.iter().chain(others.iter().copied()).collect();
    for a in &set {
        for c in &a.conflicts {
            let (cid, creq) = parse_ref(c).map_err(|e| fail("bad_requirement", format!("{}: {e}", a.key)))?;
            if let Some(b) = everyone.iter().find(|b| b.id == cid && creq.matches(&b.version())) {
                let where_ = if set.iter().any(|s| s.key == b.key) {
                    "in the same load"
                } else {
                    "resident"
                };
                return Err(fail(
                    "conflict",
                    format!("{} conflicts with {c}, and {} is {where_}", a.key, b.key),
                ));
            }
        }
    }
    for b in &set {
        for a in &others {
            for c in &a.conflicts {
                if let Ok((cid, creq)) = parse_ref(c) {
                    if cid == b.id && creq.matches(&b.version()) {
                        return Err(fail(
                            "conflict",
                            format!(
                                "the resident {} conflicts with {c}, and {} is part of this load",
                                a.key, b.key
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::km::manifest::Manifest;
    use crate::km::pack::{SigState, Source};
    use crate::km::sample::Builder;
    use crate::km::store::Trust;
    use std::path::Path;

    fn add(s: &Store, root: &Path, id: &str, version: &str, requires: &[&str], conflicts: &[&str]) -> Record {
        let dir = Builder::new(root, "memory", id, version)
            .requires(requires)
            .conflicts(conflicts)
            .tables("[memory]\nchunks = \"payload/c.jsonl\"\nembeddings = \"payload/e.f16\"\ndim = 4")
            .embed_model("hash-384")
            .file(
                "payload/c.jsonl",
                "chunks",
                "jsonl",
                b"{\"id\":\"a\",\"text\":\"t\"}\n".to_vec(),
            )
            .file("payload/e.f16", "embeddings", "f16", vec![0; 8])
            .build();
        let m = Manifest::from_file(&dir.join("km.toml")).unwrap();
        let stage = s.stage_dir(&dir, &m).unwrap();
        let t = Trust {
            source: Source::Own,
            sig: SigState::Unsigned,
            overridden: false,
        };
        s.install(&stage, &m, &t, 0).unwrap().0
    }

    fn setup() -> (std::path::PathBuf, Store) {
        let home = crate::testing::temp_home();
        let s = Store::open(&home).unwrap();
        (home, s)
    }

    fn keys(v: &[Record]) -> Vec<String> {
        v.iter().map(|r| r.key.clone()).collect()
    }

    #[test]
    fn dependencies_come_first_and_the_best_version_is_chosen() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &src, "lib.a", "1.0.0", &[], &[]);
        add(&s, &src, "lib.a", "1.5.0", &[], &[]);
        add(&s, &src, "lib.a", "2.0.0", &[], &[]);
        add(&s, &src, "lib.b", "1.0.0", &["lib.a@^1"], &[]);
        let top = add(&s, &src, "app", "1.0.0", &["lib.b", "lib.a@>=1.0, <2.0"], &[]);
        let set = closure(&s, std::slice::from_ref(&top), &[]).unwrap();
        assert_eq!(
            keys(&set),
            vec!["lib.a@1.5.0", "lib.b@1.0.0", "app@1.0.0"],
            "newest satisfying, deps first"
        );
        // the stable channel wins when it satisfies
        s.channel_set("lib.a", "stable", "1.0.0", None).unwrap();
        assert_eq!(
            keys(&closure(&s, std::slice::from_ref(&top), &[]).unwrap())[0],
            "lib.a@1.0.0"
        );
        // a resident version that satisfies is reused
        let resident = s.get("lib.a@1.5.0").unwrap().unwrap();
        assert_eq!(keys(&closure(&s, &[top], &[resident]).unwrap())[0], "lib.a@1.5.0");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_missing_or_unsatisfiable_requirement_says_what_is_installed() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &src, "lib.a", "1.0.0", &[], &[]);
        let missing = add(&s, &src, "app.one", "1.0.0", &["nothing.here"], &[]);
        let e = closure(&s, &[missing], &[]).unwrap_err();
        assert_eq!(e.code, "unsatisfiable");
        assert!(e.message.contains("nothing.here is not installed"), "{}", e.message);
        let wrong = add(&s, &src, "app.two", "1.0.0", &["lib.a@>=2"], &[]);
        let e = closure(&s, &[wrong], &[]).unwrap_err();
        assert!(e.message.contains("only 1.0.0 of lib.a is installed"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn incompatible_ranges_on_one_km_are_refused() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &src, "lib.a", "1.0.0", &[], &[]);
        add(&s, &src, "lib.a", "2.0.0", &[], &[]);
        add(&s, &src, "x", "1.0.0", &["lib.a@^1"], &[]);
        add(&s, &src, "y", "1.0.0", &["lib.a@^2"], &[]);
        let both = add(&s, &src, "both", "1.0.0", &["x", "y"], &[]);
        let e = closure(&s, &[both], &[]).unwrap_err();
        assert_eq!(e.code, "unsatisfiable");
        assert!(
            e.message.contains("y@1.0.0 requires lib.a@^2") && e.message.contains("lib.a@1.0.0"),
            "{}",
            e.message
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_cycle_is_named() {
        let (home, s) = setup();
        let src = home.join("src");
        add(&s, &src, "cyc.a", "1.0.0", &["cyc.b"], &[]);
        add(&s, &src, "cyc.b", "1.0.0", &["cyc.c"], &[]);
        let c = add(&s, &src, "cyc.c", "1.0.0", &["cyc.a"], &[]);
        let e = closure(&s, &[s.get("cyc.a@1.0.0").unwrap().unwrap()], &[]).unwrap_err();
        assert_eq!(e.code, "cycle");
        assert!(
            e.message.contains("cyc.a@1.0.0 -> cyc.b@1.0.0 -> cyc.c@1.0.0 -> cyc.a"),
            "{}",
            e.message
        );
        assert_eq!(closure(&s, &[c], &[]).unwrap_err().code, "cycle");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn conflicts_are_refused_in_the_set_and_against_resident_kms() {
        let (home, s) = setup();
        let src = home.join("src");
        let a = add(&s, &src, "style.formal", "1.0.0", &[], &["style.casual@>=1"]);
        let b = add(&s, &src, "style.casual", "1.2.0", &[], &[]);
        let both = add(&s, &src, "pair", "1.0.0", &["style.formal", "style.casual"], &[]);
        let e = closure(&s, &[both], &[]).unwrap_err();
        assert_eq!(e.code, "conflict");
        assert!(
            e.message.contains("style.formal@1.0.0 conflicts with style.casual@>=1"),
            "{}",
            e.message
        );
        // against what is resident, in both directions
        let e = closure(&s, std::slice::from_ref(&a), std::slice::from_ref(&b)).unwrap_err();
        assert!(e.message.contains("style.casual@1.2.0 is resident"), "{}", e.message);
        let e = closure(&s, &[b], &[a]).unwrap_err();
        assert!(
            e.message.contains("the resident style.formal@1.0.0 conflicts"),
            "{}",
            e.message
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_range_a_conflict_does_not_cover_is_fine() {
        let (home, s) = setup();
        let src = home.join("src");
        let a = add(&s, &src, "m.a", "1.0.0", &[], &["m.b@>=2"]);
        let b = add(&s, &src, "m.b", "1.0.0", &[], &[]);
        assert!(closure(&s, &[a], &[b]).is_ok());
        let _ = std::fs::remove_dir_all(&home);
    }
}

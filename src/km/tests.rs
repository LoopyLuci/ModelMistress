//! Integration tests: the KM runtime on a real home folder, through the hub's operations and routes.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::Body;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::embed::{Embedder, HashEmbedder};
use super::pack::{self, Source};
use super::residency::tests_support::Recorder;
use super::sample::{self, Builder};
use super::*;
use crate::engine::Settings;
use crate::hub::{self, Hub};

fn setup() -> (PathBuf, Hub, Arc<Km>, Arc<Recorder>) {
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let km = runtime(&hub.engine).unwrap();
    let rec = Arc::new(Recorder::default());
    km.set_attacher(Some(rec.clone()));
    (home, hub, km, rec)
}

fn settings(hub: &Hub, f: impl FnOnce(&mut KmSettings)) {
    let mut s: Settings = hub.engine.settings();
    f(&mut s.km);
    hub.engine.set_settings(s).unwrap();
}

async fn call(hub: &Hub, op: &str, a: Value) -> Result<Value, Fail> {
    super::ops::call(hub, op, &a).await
}

fn import(km: &Km, dir: &Path) -> Value {
    km.import_sync(&dir.display().to_string(), Source::Own, false, None)
        .unwrap()
}

fn adapter(home: &Path, id: &str, v: &str, descriptor: &str) -> PathBuf {
    sample::adapter(&home.join("src"), id, v, descriptor)
}

#[tokio::test]
async fn import_export_and_import_again_from_the_archive() {
    let (home, _hub, km, _) = setup();
    let dir = adapter(&home, "code.rust", "1.0.0", "rust async");
    let r = import(&km, &dir);
    assert_eq!(r["key"], "code.rust@1.0.0");
    assert_eq!(r["already_installed"], false);
    assert_eq!(r["unapproved_tags"].as_array().unwrap().len(), 0, "{r}");
    assert_eq!(import(&km, &dir)["already_installed"], true);
    let out = km
        .export_sync(
            "code.rust",
            Some("1.0.0"),
            None,
            Some(&home.join("out/a.mmkm").display().to_string()),
        )
        .unwrap();
    assert!(Path::new(out["path"].as_str().unwrap()).is_file());
    let home2 = crate::testing::temp_home();
    let hub2 = crate::testing::hub(&home2, &[]);
    let km2 = runtime(&hub2.engine).unwrap();
    let r2 = km2
        .import_sync(out["path"].as_str().unwrap(), Source::Own, false, Some("stable"))
        .unwrap();
    assert_eq!(r2["content_id"], r["content_id"]);
    assert_eq!(km2.store.channel_get("code.rust", "stable").as_deref(), Some("1.0.0"));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&home2);
}

#[tokio::test]
async fn the_signature_policy_decides_peer_and_download_imports() {
    let (home, hub, km, _) = setup();
    let dir = adapter(&home, "code.rust", "1.0.0", "x");
    let path = dir.display().to_string();
    let e = km.import_sync(&path, Source::Peer, false, None).unwrap_err();
    assert_eq!(e.code, "untrusted", "{}", e.message);
    assert!(km.store.get("code.rust@1.0.0").unwrap().is_none());
    assert_eq!(km.manager.events().recent(10, None, Some("refuse")).len(), 1);
    let r = km.import_sync(&path, Source::Peer, true, None).unwrap();
    assert_eq!(r["unsigned_override"], true);
    assert!(km.store.get("code.rust@1.0.0").unwrap().unwrap().overridden);
    let signed = adapter(&home, "code.go", "1.0.0", "go");
    let pubkey = pack::sign_dir(&signed, [9u8; 32]).unwrap();
    let e = km
        .import_sync(&signed.display().to_string(), Source::Download, false, None)
        .unwrap_err();
    assert!(e.message.contains("untrusted key"), "{}", e.message);
    settings(&hub, |k| k.trusted_keys = vec![pubkey.clone()]);
    let r = km
        .import_sync(&signed.display().to_string(), Source::Download, false, None)
        .unwrap();
    assert_eq!(r["signature"]["state"], "trusted");
    assert_eq!(r["unsigned_override"], false);
    let broken = adapter(&home, "code.bad", "1.0.0", "bad");
    pack::sign_dir(&broken, [9u8; 32]).unwrap();
    let mut t = std::fs::read_to_string(broken.join("km.toml")).unwrap();
    t.push_str("# edited\n");
    std::fs::write(broken.join("km.toml"), t).unwrap();
    let e = km
        .import_sync(&broken.display().to_string(), Source::Own, true, None)
        .unwrap_err();
    assert_eq!(e.code, "bad_signature");
    // a loaded download is re-checked against today's trusted keys
    settings(&hub, |k| k.trusted_keys = vec![]);
    let e = km.load(LoadReq::new("code.go")).await.unwrap_err();
    assert!(e.message.contains("trust"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_tampered_store_is_caught_on_load_and_the_load_changes_nothing() {
    let (home, _hub, km, rec) = setup();
    import(&km, &adapter(&home, "code.rust", "1.0.0", "x"));
    let f = km.store.dir_of("code.rust@1.0.0").join("payload/adapter.gguf");
    let mut b = std::fs::read(&f).unwrap();
    b[5] ^= 1;
    std::fs::write(&f, b).unwrap();
    let mut req = LoadReq::new("code.rust");
    req.core = Some("dusty".into());
    let e = km.load(req).await.unwrap_err();
    assert!(
        e.message.contains("verification failed") && e.message.contains("sha256"),
        "{}",
        e.message
    );
    assert!(km.manager.resident_keys().is_empty());
    assert!(rec.log.lock().is_empty(), "nothing was attached");
    assert!(km.manager.events().recent(10, None, Some("verify"))[0]
        .reason
        .starts_with("failed"));
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn adapters_attach_to_a_core_and_detach_on_unload() {
    let (home, hub, km, rec) = setup();
    import(&km, &adapter(&home, "code.rust", "1.0.0", "x"));
    let out = call(
        &hub,
        "km.load",
        json!({"id": "code.rust", "core": "dusty", "scale": 0.5}),
    )
    .await
    .unwrap();
    assert_eq!(out["loaded"][0]["tier"], "vram");
    assert_eq!(out["loaded"][0]["attach"], "attached");
    assert!(rec.log.lock()[0].starts_with("attach dusty") && rec.log.lock()[0].ends_with(" 0.5"));
    let r = call(&hub, "km.residency", json!({})).await.unwrap();
    assert_eq!(r["resident"][0]["key"], "code.rust@1.0.0");
    assert_eq!(r["tiers"]["vram"]["count"], 1);
    let e = call(&hub, "km.remove", json!({"id": "code.rust", "version": "1.0.0"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, "resident");
    assert!(km.store.get("code.rust@1.0.0").unwrap().is_some());
    assert!(km.attached_file().is_file());
    call(&hub, "km.unload", json!({"id": "code.rust"})).await.unwrap();
    assert!(rec.log.lock()[1].starts_with("detach dusty"));
    call(&hub, "km.remove", json!({"id": "code.rust", "version": "1.0.0"}))
        .await
        .unwrap();
    assert!(call(&hub, "km.list", json!({}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let kinds: Vec<String> = call(&hub, "km.events", json!({})).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    for k in ["verify", "load", "unload", "detach"] {
        assert!(kinds.contains(&k.to_string()), "{k} in {kinds:?}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn without_a_core_an_adapter_is_staged_in_ram() {
    let (home, _hub, km, rec) = setup();
    import(&km, &adapter(&home, "code.rust", "1.0.0", "x"));
    let out = km.load(LoadReq::new("code.rust")).await.unwrap();
    assert_eq!(out["loaded"][0]["tier"], "ram");
    assert_eq!(out["loaded"][0]["attach"], "staged");
    assert!(rec.log.lock().is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_misdeclared_km_is_marked_refused_and_no_longer_selected() {
    let (home, hub, km, _) = setup();
    let dir = Builder::new(&home.join("src"), "adapter", "liar", "1.0.0")
        .descriptor("liar adapter")
        .declared(100, 100)
        .file("payload/a.gguf", "lora", "gguf", sample::filler("liar", 2048))
        .build();
    import(&km, &dir);
    let mut req = LoadReq::new("liar");
    req.core = Some("dusty".into());
    let e = km.load(req.clone()).await.unwrap_err();
    assert!(
        e.message.contains("misdeclared") && e.message.contains("declared ram_bytes 100"),
        "{}",
        e.message
    );
    assert!(km.store.get("liar@1.0.0").unwrap().unwrap().misdeclared.is_some());
    assert!(!km.manager.events().recent(10, None, Some("misdeclared")).is_empty());
    let e = km.load(req.clone()).await.unwrap_err();
    assert!(e.message.contains("marked misdeclared"), "{}", e.message);
    let s = call(&hub, "km.search", json!({"query": "liar adapter"})).await.unwrap();
    assert!(s["results"].as_array().unwrap().is_empty());
    assert!(
        s["excluded"][0]["reasons"][0]
            .as_str()
            .unwrap()
            .starts_with("misdeclared"),
        "{s}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn budgets_evict_the_coldest_and_pins_survive() {
    let (home, hub, km, _) = setup();
    for (id, d) in [("a.one", "one"), ("a.two", "two"), ("a.three", "three")] {
        import(&km, &sample::adapter_sized(&home.join("src"), id, "1.0.0", d, 2000));
    }
    let sz = km.store.get("a.one@1.0.0").unwrap().unwrap().bytes;
    settings(&hub, |k| k.budget_ram_bytes = sz * 2 + 10);
    call(&hub, "km.load", json!({"id": "a.one"})).await.unwrap();
    call(&hub, "km.load", json!({"id": "a.two"})).await.unwrap();
    call(&hub, "km.pin", json!({"id": "a.one"})).await.unwrap();
    let out = call(&hub, "km.load", json!({"id": "a.three"})).await.unwrap();
    assert_eq!(out["loaded"][0]["evicted"], json!(["a.two@1.0.0"]));
    call(&hub, "km.pin", json!({"id": "a.three"})).await.unwrap();
    let e = call(&hub, "km.load", json!({"id": "a.two"})).await.unwrap_err();
    assert!(e.message.contains("(pinned)"), "{}", e.message);
    assert_eq!(km.manager.resident_keys(), vec!["a.one@1.0.0", "a.three@1.0.0"]);
    let r = call(&hub, "km.residency", json!({})).await.unwrap();
    assert!(r["tiers"]["ram"]["used"].as_u64().unwrap() <= sz * 2 + 10);
    assert_eq!(r["pinned"], json!(["a.one", "a.three"]));
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_requests_set_is_protected_while_its_guard_lives() {
    let (home, hub, km, _) = setup();
    for id in ["p.one", "p.two", "p.three"] {
        import(&km, &sample::adapter_sized(&home.join("src"), id, "1.0.0", id, 1000));
    }
    let sz = km.store.get("p.one@1.0.0").unwrap().unwrap().bytes;
    settings(&hub, |k| k.budget_ram_bytes = sz * 2 + 10);
    let guard = km
        .open_request(&[LoadReq::new("p.one"), LoadReq::new("p.two")])
        .await
        .unwrap();
    assert_eq!(guard.keys().len(), 2);
    let e = km.load(LoadReq::new("p.three")).await.unwrap_err();
    assert!(e.message.contains("(in flight)"), "{}", e.message);
    drop(guard);
    assert!(km.load(LoadReq::new("p.three")).await.is_ok());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn requires_load_together_and_failures_are_explained_and_rolled_back() {
    let (home, _hub, km, _) = setup();
    let src = home.join("src");
    import(&km, &sample::memory(&src, "docs.base", "1.0.0", &[("c", "base facts")]));
    import(
        &km,
        &Builder::new(&src, "adapter", "skill.top", "1.0.0")
            .descriptor("top")
            .requires(&["docs.base@^1"])
            .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
            .build(),
    );
    let out = km.load(LoadReq::new("skill.top")).await.unwrap();
    let keys: Vec<&str> = out["loaded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["docs.base@1.0.0", "skill.top@1.0.0"], "dependency first");
    km.unload("skill.top", None, false).await.unwrap();
    assert_eq!(km.manager.resident_keys(), vec!["docs.base@1.0.0"]);
    km.unload("", None, true).await.unwrap();
    import(
        &km,
        &Builder::new(&src, "adapter", "skill.broken", "1.0.0")
            .descriptor("broken")
            .requires(&["nothing.here"])
            .file("payload/a.gguf", "lora", "gguf", vec![2; 64])
            .build(),
    );
    let e = km.load(LoadReq::new("skill.broken")).await.unwrap_err();
    assert!(e.message.contains("nothing.here is not installed"), "{}", e.message);
    assert!(km.manager.resident_keys().is_empty());
    import(
        &km,
        &Builder::new(&src, "adapter", "style.formal", "1.0.0")
            .descriptor("formal")
            .conflicts(&["docs.base"])
            .file("payload/a.gguf", "lora", "gguf", vec![3; 64])
            .build(),
    );
    km.load(LoadReq::new("docs.base")).await.unwrap();
    let e = km.load(LoadReq::new("style.formal")).await.unwrap_err();
    assert_eq!(e.code, "conflict", "{}", e.message);
    // rollback: the dependency loads, then the requester fails verification
    km.unload("", None, true).await.unwrap();
    import(
        &km,
        &Builder::new(&src, "adapter", "skill.late", "1.0.0")
            .descriptor("late")
            .requires(&["docs.base"])
            .file("payload/a.gguf", "lora", "gguf", vec![4; 64])
            .build(),
    );
    let f = km.store.dir_of("skill.late@1.0.0").join("payload/a.gguf");
    std::fs::write(&f, b"tampered").unwrap();
    let e = km.load(LoadReq::new("skill.late")).await.unwrap_err();
    assert!(e.message.contains("verification failed"), "{}", e.message);
    assert!(km.manager.resident_keys().is_empty(), "docs.base was rolled back");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_bundle_loads_and_unloads_its_members() {
    let (home, _hub, km, _) = setup();
    let src = home.join("src");
    import(&km, &sample::memory(&src, "docs.a", "1.0.0", &[("c", "alpha")]));
    import(&km, &sample::memory(&src, "docs.b", "1.0.0", &[("c", "beta")]));
    import(&km, &sample::bundle(&src, "pack.ab", "1.0.0", &["docs.a@^1", "docs.b"]));
    let out = km.load(LoadReq::new("pack.ab")).await.unwrap();
    assert_eq!(out["loaded"].as_array().unwrap().len(), 3);
    km.unload("pack.ab", None, false).await.unwrap();
    assert!(km.manager.resident_keys().is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn experts_projector_and_router_are_held_with_attach_pending_and_a_specialist_is_attached() {
    let (home, _hub, km, _) = setup();
    let src = home.join("src");
    import(&km, &sample::experts(&src, "ex.rust", "1.0.0", "experts"));
    import(&km, &sample::router(&src, "router.main", "1.0.0"));
    import(&km, &sample::specialist(&src, "spec.tts", "1.0.0", "tts", "some-tts"));
    import(&km, &sample::projector(&src, "proj.vis", "1.0.0", "spec.tts"));
    for id in ["ex.rust", "router.main", "spec.tts", "proj.vis"] {
        let out = km.load(LoadReq::new(id)).await.unwrap();
        // a specialist is callable (km.specialist.call); the others wait for their assembly into a core
        let want = if id == "spec.tts" { "attached" } else { "pending" };
        assert_eq!(out["loaded"][0]["attach"], want, "{id}");
        assert_eq!(out["loaded"][0]["tier"], "ram");
    }
    assert_eq!(km.manager.resident_keys().len(), 4);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn tool_kms_refer_to_what_the_host_has_and_grant_nothing() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    import(&km, &sample::tool(&src, "tool.models", "1.0.0", "hub", "model.list"));
    let out = km.load(LoadReq::new("tool.models")).await.unwrap();
    assert_eq!(out["loaded"][0]["attach"], "pending");
    let s = call(&hub, "km.show", json!({"id": "tool.models"})).await.unwrap();
    assert_eq!(s["tool"]["grants_permission"], false);
    assert_eq!(s["tool"]["name"], "model.list");
    import(
        &km,
        &sample::tool(&src, "tool.ghost", "1.0.0", "hub", "no.such.operation"),
    );
    let e = km.load(LoadReq::new("tool.ghost")).await.unwrap_err();
    assert!(e.message.contains("not a hub operation"), "{}", e.message);
    import(&km, &sample::tool(&src, "tool.mcp", "1.0.0", "mcp:files", "read_file"));
    assert!(km.load(LoadReq::new("tool.mcp")).await.is_ok());
    for extra in [
        "permissions = [\"write\"]",
        "command = \"rm -rf\"",
        "url = \"http://x\"",
        "token = \"abc\"",
    ] {
        let dir = Builder::new(&src, "tool", "tool.evil", "1.0.0")
            .descriptor("evil")
            .tables(&format!("[tool]\nprovider = \"hub\"\nname = \"model.list\"\n{extra}"))
            .build();
        let e = km
            .import_sync(&dir.display().to_string(), Source::Own, false, None)
            .unwrap_err();
        assert!(e.message.contains("not allowed"), "{extra}: {}", e.message);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    let dir = Builder::new(&src, "tool", "tool.exe", "1.0.0")
        .descriptor("exe")
        .top("permissions = [\"all\"]")
        .tables("[tool]\nprovider = \"hub\"\nname = \"model.list\"")
        .file("payload/run.sh", "script", "sh", b"echo".to_vec())
        .build();
    let e = km
        .import_sync(&dir.display().to_string(), Source::Own, false, None)
        .unwrap_err();
    assert!(
        e.message.contains("permissions is not allowed") && e.message.contains(".sh"),
        "{}",
        e.message
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn skill_kms_name_their_instructions_examples_and_tools() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    import(
        &km,
        &sample::skill(&src, "skill.review", "1.0.0", Some("data.review@^1"), &["tool.models"]),
    );
    let s = call(&hub, "km.show", json!({"id": "skill.review"})).await.unwrap();
    assert_eq!(s["skill"]["instructions"], "payload/SKILL.md");
    assert_eq!(s["skill"]["tools"], json!(["tool.models"]));
    let bad = Builder::new(&src, "skill", "skill.bad", "1.0.0")
        .tables("[skill]\ninstructions = \"payload/none.md\"")
        .build();
    assert!(km
        .import_sync(&bad.display().to_string(), Source::Own, false, None)
        .is_err());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn consolidation_state_is_stored_shown_and_steers_selection() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    let dir = Builder::new(&src, "adapter", "skill.done", "1.0.0")
        .descriptor("already learned skill")
        .tables("[consolidation.dusty]\nstate = \"absorbed\"\nuplift = 0.004\nsteps = 9\nlast_step = \"run-7\"")
        .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
        .build();
    import(&km, &dir);
    let s = call(&hub, "km.show", json!({"id": "skill.done"})).await.unwrap();
    assert_eq!(s["consolidation"]["dusty"]["state"], "absorbed");
    assert_eq!(s["consolidation"]["dusty"]["steps"], 9);
    let r = call(&hub, "km.search", json!({"query": "already learned skill"}))
        .await
        .unwrap();
    assert!(r["results"].as_array().unwrap().is_empty());
    assert!(
        r["excluded"][0]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("absorbed into dusty's weights"),
        "{r}"
    );
    let r = call(
        &hub,
        "km.search",
        json!({"query": "already learned skill", "include_absorbed": true}),
    )
    .await
    .unwrap();
    assert_eq!(r["results"][0]["id"], "skill.done");
    let bad = Builder::new(&src, "adapter", "skill.odd", "1.0.0")
        .tables("[consolidation]\nstate = \"digested\"")
        .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
        .build();
    assert!(km
        .import_sync(&bad.display().to_string(), Source::Own, false, None)
        .unwrap_err()
        .message
        .contains("consolidation.state"));
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn tags_carry_origin_and_confidence_and_the_vocabulary_is_editable() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    let dir = Builder::new(&src, "adapter", "code.tagged", "1.0.0")
        .descriptor("tagged")
        .tags(&["lang:rust", "task:dancing", "odd"])
        .top("[tag_info]\n\"lang:rust\" = { origin = \"classifier\", confidence = 0.8 }\n\"task:dancing\" = { origin = \"author_model\", confidence = 0.3 }")
        .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
        .build();
    let r = import(&km, &dir);
    let un: Vec<String> = r["unapproved_tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tag"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(un, vec!["task:dancing", "odd"], "{r}");
    let s = call(&hub, "km.show", json!({"id": "code.tagged"})).await.unwrap();
    let tag = |s: &Value, n: &str| {
        s["tags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["tag"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(tag(&s, "lang:rust")["origin"], "classifier");
    assert_eq!(tag(&s, "lang:rust")["confidence"], 0.8);
    assert_eq!(tag(&s, "lang:rust")["approved"], true);
    assert_eq!(tag(&s, "task:dancing")["approved"], false);
    call(
        &hub,
        "km.vocabulary.edit",
        json!({"namespace": "task", "add": ["dancing"]}),
    )
    .await
    .unwrap();
    let v = call(&hub, "km.vocabulary.get", json!({})).await.unwrap();
    assert!(v["namespaces"]["task"]["values"]
        .as_array()
        .unwrap()
        .contains(&json!("dancing")));
    assert_eq!(v["custom"], true);
    let s = call(&hub, "km.show", json!({"id": "code.tagged"})).await.unwrap();
    assert_eq!(tag(&s, "task:dancing")["approved"], true);
    let r = call(
        &hub,
        "km.search",
        json!({"query": "tag:task:dancing", "min_tag_confidence": 0.5}),
    )
    .await
    .unwrap();
    assert!(r["results"].as_array().unwrap().is_empty());
    assert!(
        r["excluded"][0]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("confidence 0.30 (author_model)"),
        "{r}"
    );
    let r = call(&hub, "km.search", json!({"query": "tag:task:dancing"}))
        .await
        .unwrap();
    assert_eq!(r["results"][0]["terms"]["tags"]["matched"], 1);
    let l = call(&hub, "km.list", json!({"tag": "lang:rust"})).await.unwrap();
    assert_eq!(l.as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn search_explains_and_the_embed_guard_works_through_the_runtime() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    import(
        &km,
        &sample::adapter(
            &src,
            "code.rust.async",
            "1.0.0",
            "async rust tokio futures cancellation",
        ),
    );
    import(&km, &sample::adapter(&src, "code.sql", "1.0.0", "sql postgres joins"));
    let r = call(&hub, "km.search", json!({"query": "cancel a tokio future", "top_k": 5}))
        .await
        .unwrap();
    assert_eq!(r["results"][0]["id"], "code.rust.async");
    assert_eq!(r["results"][0]["terms"]["vec"]["used"], true);
    assert_eq!(r["embedder"], "hash-384");
    struct Other;
    impl Embedder for Other {
        fn model_id(&self) -> String {
            "mm-embed-1".into()
        }
        fn embed<'a>(&'a self, t: &'a [String]) -> futures::future::BoxFuture<'a, Res<Vec<Vec<f32>>>> {
            Box::pin(async move { Ok(t.iter().map(|_| vec![1.0; 384]).collect()) })
        }
    }
    km.set_embedder(Some(Arc::new(Other)));
    let r = call(&hub, "km.search", json!({"query": "cancel a tokio future"}))
        .await
        .unwrap();
    let top = &r["results"][0];
    assert_eq!(top["id"], "code.rust.async", "the lexical term still ranks it");
    assert_eq!(top["terms"]["vec"]["used"], false);
    let note = top["notes"][0].as_str().unwrap();
    assert!(note.contains("hash-384") && note.contains("mm-embed-1"), "{note}");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn memory_queries_return_cited_chunks_and_guard_the_embedder() {
    let (home, hub, km, _) = setup();
    import(
        &km,
        &sample::memory(
            &home.join("src"),
            "docs.rust",
            "1.0.0",
            &[
                ("c1", "tokio tasks are cancelled by dropping the handle"),
                ("c2", "the borrow checker tracks lifetimes"),
            ],
        ),
    );
    let r = call(
        &hub,
        "km.memory.query",
        json!({"id": "docs.rust", "text": "cancel a tokio task", "top_k": 1}),
    )
    .await
    .unwrap();
    assert_eq!(r["hits"][0]["chunk"], "c1");
    assert_eq!(r["hits"][0]["citation"], "docs.rust@1.0.0#c1");
    assert!(r["hits"][0]["score"].as_f64().unwrap() > 0.2);
    assert!(km.manager.is_resident("docs.rust@1.0.0"));
    assert_eq!(km.manager.refs_of("docs.rust@1.0.0"), 0, "the query's hold is released");
    let v = HashEmbedder::default().embed_one("borrow checker lifetimes");
    let r = call(
        &hub,
        "km.memory.query",
        json!({"id": "docs.rust", "vector": v, "top_k": 1}),
    )
    .await
    .unwrap();
    assert_eq!(r["hits"][0]["chunk"], "c2");
    struct Other;
    impl Embedder for Other {
        fn model_id(&self) -> String {
            "other".into()
        }
        fn embed<'a>(&'a self, _: &'a [String]) -> futures::future::BoxFuture<'a, Res<Vec<Vec<f32>>>> {
            Box::pin(async { Ok(vec![vec![0.0; 384]]) })
        }
    }
    km.set_embedder(Some(Arc::new(Other)));
    let e = call(&hub, "km.memory.query", json!({"id": "docs.rust", "text": "x"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, "embed_model_mismatch");
    import(&km, &sample::adapter(&home.join("src"), "code.rust", "1.0.0", "x"));
    assert!(call(&hub, "km.memory.query", json!({"id": "code.rust", "text": "x"}))
        .await
        .is_err());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn data_records_can_be_edited_approved_and_committed_through_the_operations() {
    let (home, hub, km, _) = setup();
    import(
        &km,
        &sample::data(
            &home.join("src"),
            "data.rust",
            "1.0.0",
            &[
                sample::record("r1", "q1", "a1", "pending"),
                sample::record("r2", "q2", "a2", "pending"),
            ],
        ),
    );
    let l = call(&hub, "km.data.list", json!({"id": "data.rust", "status": "pending"}))
        .await
        .unwrap();
    assert_eq!(l["matching"], 2);
    assert_eq!(l["counts"]["pending"], 2);
    let e = call(
        &hub,
        "km.data.edit",
        json!({"id": "data.rust", "record": "r1", "changes": {"output": "better"}}),
    )
    .await
    .unwrap();
    assert_eq!(e["rev"], 2);
    call(&hub, "km.data.approve", json!({"id": "data.rust", "records": ["r1"]}))
        .await
        .unwrap();
    call(
        &hub,
        "km.data.reject",
        json!({"id": "data.rust", "from_status": "pending"}),
    )
    .await
    .unwrap();
    let g = call(
        &hub,
        "km.data.get",
        json!({"id": "data.rust", "record": "r1", "history": true}),
    )
    .await
    .unwrap();
    assert_eq!(g["status"], "approved");
    assert_eq!(g["rev"], 3);
    assert_eq!(g["history"].as_array().unwrap().len(), 3);
    assert_eq!(
        call(&hub, "km.data.get", json!({"id": "data.rust", "record": "r2"}))
            .await
            .unwrap()["status"],
        "rejected"
    );
    let c = call(&hub, "km.data.commit", json!({"id": "data.rust"})).await.unwrap();
    assert_eq!(c["committed"], "data.rust@1.0.1");
    assert_eq!(c["parent"], "data.rust@1.0.0");
    let s = call(&hub, "km.show", json!({"id": "data.rust", "version": "1.0.1"}))
        .await
        .unwrap();
    assert_eq!(s["data"]["counts"]["approved"], 1);
    assert_eq!(
        call(&hub, "km.list", json!({"id": "data.rust"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    import(&km, &adapter(&home, "code.rust", "1.0.0", "x"));
    assert!(call(&hub, "km.data.list", json!({"id": "code.rust"})).await.is_err());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn channels_pins_bans_and_prefetch_through_the_operations() {
    let (home, hub, km, _) = setup();
    let src = home.join("src");
    import(&km, &sample::adapter(&src, "code.rust", "1.0.0", "rust one"));
    import(&km, &sample::adapter(&src, "code.rust", "1.1.0", "rust two"));
    import(&km, &sample::memory(&src, "docs.a", "1.0.0", &[("c", "alpha")]));
    import(&km, &sample::memory(&src, "docs.b", "1.0.0", &[("c", "beta")]));
    call(
        &hub,
        "km.channel.set",
        json!({"id": "code.rust", "channel": "stable", "version": "1.0.0"}),
    )
    .await
    .unwrap();
    call(
        &hub,
        "km.channel.set",
        json!({"id": "code.rust", "channel": "candidate", "version": "1.1.0"}),
    )
    .await
    .unwrap();
    let e = call(
        &hub,
        "km.channel.set",
        json!({"id": "code.rust", "channel": "stable", "version": "9.9.9"}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.status, 404);
    let g = call(&hub, "km.channel.get", json!({"id": "code.rust"})).await.unwrap();
    assert_eq!(g["channels"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(&hub, "km.channel.get", json!({"id": "code.rust", "channel": "stable"}))
            .await
            .unwrap()["version"],
        "1.0.0"
    );
    let out = call(&hub, "km.load", json!({"id": "code.rust", "channel": "candidate"}))
        .await
        .unwrap();
    assert_eq!(out["requested"], "code.rust@1.1.0");
    let out = call(&hub, "km.load", json!({"id": "code.rust"})).await.unwrap();
    assert_eq!(out["requested"], "code.rust@1.0.0");
    assert_eq!(km.manager.resident_keys(), vec!["code.rust@1.0.0"]);
    let l = call(&hub, "km.list", json!({"id": "code.rust"})).await.unwrap();
    let row = l
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["version"] == "1.0.0")
        .unwrap()
        .clone();
    assert_eq!(row["channels"], json!(["stable"]));
    assert_eq!(row["resident"], true);
    let b = call(&hub, "km.ban", json!({"id": "code.rust", "reason": "wrong"}))
        .await
        .unwrap();
    assert_eq!(b["unloaded"], json!(["code.rust@1.0.0"]));
    assert!(call(&hub, "km.load", json!({"id": "code.rust"}))
        .await
        .unwrap_err()
        .message
        .contains("banned"));
    call(&hub, "km.ban", json!({"id": "code.rust", "banned": false}))
        .await
        .unwrap();
    let p = call(&hub, "km.prefetch", json!({"ids": ["docs.a", "docs.b", "nope"]}))
        .await
        .unwrap();
    assert_eq!(p[0]["result"], "warmed");
    assert_eq!(p[2]["result"], "skipped");
    let a_bytes = km.store.get("docs.a@1.0.0").unwrap().unwrap().bytes;
    settings(&hub, |k| k.budget_ram_bytes = a_bytes + 10);
    km.unload("docs.b", None, false).await.unwrap();
    let p = call(&hub, "km.prefetch", json!({"ids": ["docs.b"]})).await.unwrap();
    assert_eq!(p[0]["result"], "skipped", "{p}");
    assert!(p[0]["reason"].as_str().unwrap().contains("no room"));
    assert!(km.manager.is_resident("docs.a@1.0.0"), "prefetch evicted nothing");
    km.unload("", None, true).await.unwrap();
    settings(&hub, |k| k.budget_ram_bytes = 1 << 30);
    let p = call(
        &hub,
        "km.prefetch",
        json!({"query": "alpha beta", "kinds": ["memory"], "skip": 1, "count": 1}),
    )
    .await
    .unwrap();
    assert_eq!(p.as_array().unwrap().len(), 1);
    assert_eq!(p[0]["result"], "warmed");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn show_reports_everything_about_a_km() {
    let (home, hub, km, _) = setup();
    let dir = Builder::new(&home.join("src"), "adapter", "code.rust", "1.0.0")
        .descriptor("rust")
        .top("future_key = 1")
        .file("payload/a.gguf", "lora", "gguf", vec![1; 64])
        .build();
    import(&km, &dir);
    let s = call(&hub, "km.show", json!({"id": "code.rust@1.0.0"})).await.unwrap();
    assert_eq!(s["key"], "code.rust@1.0.0");
    assert_eq!(s["unknown_keys"], json!(["future_key"]));
    assert_eq!(s["manifest"]["future_key"], 1);
    assert_eq!(s["signature"]["state"], "unsigned");
    assert_eq!(s["requires_check"]["ok"], true);
    assert_eq!(s["payload"][0]["role"], "lora");
    assert!(s["history"].as_array().unwrap().iter().any(|h| h["action"] == "import"));
    assert_eq!(
        call(&hub, "km.show", json!({"id": "nope"})).await.unwrap_err().status,
        404
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn settings_round_trip_through_config_toml() {
    let home = crate::testing::temp_home();
    let mut s = Settings::default();
    s.km.trusted_keys = vec!["ab".repeat(32)];
    s.km.core_budgets.insert(
        "dusty".into(),
        CoreBudget {
            ram_bytes: 5,
            vram_bytes: 7,
        },
    );
    s.km.embed_model = Some("mm-embed-1".into());
    s.save(&home).unwrap();
    let back = Settings::load(&home).unwrap();
    assert_eq!(back.km, s.km);
    std::fs::write(home.join("config.toml"), "max_loaded = 3\n").unwrap();
    assert_eq!(Settings::load(&home).unwrap().km, KmSettings::default());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn operations_are_unique_marked_and_have_routes() {
    let ops = hub::operations();
    let ids: Vec<&str> = ops.iter().map(|o| o["id"].as_str().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
    let kms: Vec<&Value> = ops
        .iter()
        .filter(|o| o["id"].as_str().unwrap().starts_with("km."))
        .collect();
    for want in [
        "km.list",
        "km.show",
        "km.search",
        "km.import",
        "km.export",
        "km.remove",
        "km.load",
        "km.unload",
        "km.pin",
        "km.ban",
        "km.channel.set",
        "km.channel.get",
        "km.residency",
        "km.events",
        "km.data.list",
        "km.data.get",
        "km.data.edit",
        "km.data.approve",
        "km.data.reject",
        "km.data.commit",
        "km.memory.query",
        "km.vocabulary.get",
        "km.vocabulary.edit",
        "km.prefetch",
    ] {
        assert!(kms.iter().any(|o| o["id"] == want), "{want}");
    }
    let mutating = [
        "import",
        "export",
        "remove",
        "load",
        "unload",
        "pin",
        "ban",
        "channel.set",
        "data.edit",
        "data.approve",
        "data.reject",
        "data.commit",
        "vocabulary.edit",
        "prefetch",
        "specialist.call",
    ];
    for o in &kms {
        let id = o["id"].as_str().unwrap();
        assert_eq!(
            o["mutating"].as_bool().unwrap(),
            mutating.iter().any(|m| id == format!("km.{m}")),
            "{id}"
        );
        assert_eq!(o["destructive"].as_bool().unwrap(), id == "km.remove", "{id}");
        for r in o["input_schema"]["required"].as_array().unwrap() {
            assert!(
                o["input_schema"]["properties"].get(r.as_str().unwrap()).is_some(),
                "{id} {r}"
            );
        }
    }
    assert_eq!(ops::route_of("km.channel.set"), "/v1/km/channel/set");
}

async fn http(hub: &Hub, method: &str, path: &str, auth: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {auth}"));
    let b = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    let resp = hub::router(hub.clone()).oneshot(req.body(b).unwrap()).await.unwrap();
    let st = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24).await.unwrap();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_rest_routes_mirror_the_operations_and_peers_are_kept_out() {
    let (home, hub, km, _) = setup();
    import(&km, &adapter(&home, "code.rust", "1.0.0", "rust"));
    let tok = hub.token.as_str().to_string();
    let (st, v) = http(&hub, "GET", "/v1/km/list", &tok, None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["result"][0]["key"], "code.rust@1.0.0");
    let (st, v) = http(
        &hub,
        "GET",
        "/v1/km/search?query=rust&top_k=1&kinds=adapter",
        &tok,
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["result"]["results"][0]["id"], "code.rust");
    let (st, v) = http(
        &hub,
        "POST",
        "/v1/km/channel/set",
        &tok,
        Some(json!({"id": "code.rust", "channel": "stable", "version": "1.0.0"})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, _) = http(&hub, "GET", "/v1/km/channel/set?id=code.rust", &tok, None).await;
    assert_eq!(st, 405, "a mutating operation has no GET route");
    let (st, v) = http(&hub, "POST", "/v1/km/load", &tok, Some(json!({"id": "code.rust"}))).await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = http(&hub, "GET", "/v1/km/residency", &tok, None).await;
    assert_eq!(st, 200);
    assert_eq!(v["result"]["resident"][0]["key"], "code.rust@1.0.0");
    let (st, v) = http(&hub, "POST", "/v1/call/km.events", &tok, Some(json!({}))).await;
    assert_eq!(st, 200);
    assert!(v["result"]["events"].as_array().unwrap().len() >= 2);
    let (st, v) = http(&hub, "GET", "/v1/km/show?id=nope", &tok, None).await;
    assert_eq!(st, 404);
    assert!(v["error"]["message"].as_str().unwrap().contains("nope"));
    let (_, secret) = hub
        .keys
        .create("friend", crate::hosting::Quotas::default(), vec![])
        .unwrap();
    for (m, p) in [
        ("GET", "/v1/km/list"),
        ("POST", "/v1/km/load"),
        ("POST", "/v1/km/import"),
        ("GET", "/v1/km/residency"),
    ] {
        let (st, v) = http(&hub, m, p, &secret, Some(json!({}))).await;
        assert_eq!(st, 403, "{p}: {v}");
    }
    let (st, _) = http(&hub, "POST", "/v1/call/km.list", &secret, Some(json!({}))).await;
    assert_eq!(st, 403);
    let (st, _) = http(&hub, "GET", "/v1/km/list", "wrong", None).await;
    assert_eq!(st, 401);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_cli_turns_subcommands_into_operations() {
    let a = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
    let (op, v) = cli::parse(&a(
        "search cancel a task --top 3 --kind adapter --tag lang:rust --include-absorbed",
    ))
    .unwrap();
    assert_eq!(op, "km.search");
    assert_eq!(v["query"], "cancel a task");
    assert_eq!(v["top_k"], 3);
    assert_eq!(v["kinds"], json!(["adapter"]));
    assert_eq!(v["tags"], json!(["lang:rust"]));
    assert_eq!(v["include_absorbed"], true);
    let (op, v) = cli::parse(&a("channel set code.rust stable 1.0.0 --expect 0.9.0")).unwrap();
    assert_eq!((op.as_str(), v["expect"].clone()), ("km.channel.set", json!("0.9.0")));
    let (op, v) = cli::parse(&a("data approve data.rust r1 r2")).unwrap();
    assert_eq!(
        (op.as_str(), v["records"].clone()),
        ("km.data.approve", json!(["r1", "r2"]))
    );
    let (op, v) = cli::parse(&[
        "data".into(),
        "edit".into(),
        "d".into(),
        "r1".into(),
        "{\"output\":\"x\"}".into(),
        "--create".into(),
    ])
    .unwrap();
    assert_eq!(op, "km.data.edit");
    assert_eq!(
        (v["changes"]["output"].clone(), v["create"].clone()),
        (json!("x"), json!(true))
    );
    let (op, v) = cli::parse(&a("vocabulary add task dancing singing")).unwrap();
    assert_eq!(
        (op.as_str(), v["add"].clone()),
        ("km.vocabulary.edit", json!(["dancing", "singing"]))
    );
    let (op, v) = cli::parse(&a("unload --all")).unwrap();
    assert_eq!((op.as_str(), v["all"].clone()), ("km.unload", json!(true)));
    assert_eq!(cli::parse(&a("unpin a")).unwrap().1["pinned"], false);
    assert!(cli::parse(&a("frobnicate")).is_err());
    assert!(cli::parse(&a("show")).unwrap_err().contains("missing a KM id"));
    assert!(cli::parse(&a("channel nope")).is_err());
}

#[tokio::test]
async fn the_cli_helpers_that_need_no_hub_work() {
    let home = crate::testing::temp_home();
    let dir = sample::adapter(&home.join("src"), "code.rust", "1.0.0", "x");
    let key = home.join("secret.key");
    cli::run(&home, vec!["keygen".into(), key.display().to_string()])
        .await
        .unwrap();
    assert!(
        cli::run(&home, vec!["keygen".into(), key.display().to_string()])
            .await
            .is_err(),
        "no overwriting a key"
    );
    cli::run(
        &home,
        vec!["sign".into(), dir.display().to_string(), key.display().to_string()],
    )
    .await
    .unwrap();
    assert_eq!(pack::check_signature(&dir, &[]).label(), "untrusted");
    let out = home.join("a.mmkm");
    cli::run(
        &home,
        vec!["pack".into(), dir.display().to_string(), out.display().to_string()],
    )
    .await
    .unwrap();
    cli::run(&home, vec!["verify".into(), out.display().to_string()])
        .await
        .unwrap();
    cli::run(&home, vec!["verify".into(), dir.display().to_string()])
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn recorder_flags_work() {
    let r = Recorder::default();
    r.fail_attach.store(true, Ordering::SeqCst);
    assert!(r.fail_attach.load(Ordering::SeqCst));
}

// ---- specialist KMs are callable -----------------------------------------------------------------------------

/// A stand-in for the engines: remembers what it was asked and answers by engine kind.
#[derive(Default)]
struct StubEngines {
    seen: parking_lot::Mutex<Vec<(String, Value)>>,
    fail: bool,
}

impl specialist::Backend for StubEngines {
    fn run<'a>(&'a self, _: &'a Hub, engine: &'a str, args: Value) -> futures::future::BoxFuture<'a, Res<Value>> {
        Box::pin(async move {
            self.seen.lock().push((engine.to_string(), args.clone()));
            if self.fail {
                return Err(Fail::unavailable("the stand-in engine is down"));
            }
            Ok(match engine {
                "stt" => json!({"text": "hello there", "language": "en"}),
                "tts" => json!({"output": args["output"], "bytes": 3}),
                "image" => json!({"files": ["a.png"], "seed": 7}),
                _ => json!({"predictions": [{"label": "cat", "score": 0.9}]}),
            })
        })
    }
}

fn spec_kms(km: &Km, src: &Path) {
    let schema = |required: &str, props: &str| {
        format!("input_schema = {{ type = \"object\", required = [{required}], properties = {{ {props} }} }}")
    };
    for (id, engine, model, extra) in [
        (
            "spec.stt",
            "stt",
            "whisper-x",
            schema("\"file\"", "file = { type = \"string\" }"),
        ),
        (
            "spec.tts",
            "tts",
            "voice-x",
            schema(
                "\"input\", \"output\"",
                "input = { type = \"string\", maxLength = 20 }, output = { type = \"string\" }",
            ),
        ),
        (
            "spec.img",
            "image",
            "draw-x",
            schema(
                "\"prompt\"",
                "prompt = { type = \"string\" }, n = { type = \"integer\", maximum = 4 }",
            ),
        ),
        ("spec.vis", "vision", "see-x", String::new()),
    ] {
        let dir = Builder::new(src, "specialist", id, "1.0.0")
            .descriptor(&format!("{engine} specialist"))
            .tables(&format!(
                "[specialist]\nengine = \"{engine}\"\nmodel = \"{model}\"\ntool_description = \"calls {model}\"\n{extra}"
            ))
            .build();
        import(km, &dir);
    }
}

fn call_trace(hub: &Hub, id: &str) -> Value {
    let tr = crate::trace::tracer(&hub.engine.home);
    tr.store().unwrap().get(id, 0).unwrap().unwrap()
}

#[tokio::test]
async fn a_loaded_specialist_kms_calls_run_on_its_engine_and_are_traced() {
    let (home, hub, km, _) = setup();
    spec_kms(&km, &home.join("src"));
    let stub = Arc::new(StubEngines::default());
    km.set_specialist_backend(Some(stub.clone()));
    let inputs = [
        ("spec.stt", "stt", json!({"file": "a.wav"}), "text"),
        ("spec.tts", "tts", json!({"input": "hi", "output": "o.wav"}), "bytes"),
        ("spec.img", "image", json!({"prompt": "a cat", "n": 2}), "files"),
        ("spec.vis", "vision", json!({"file": "c.png"}), "predictions"),
    ];
    for (id, engine, input, field) in inputs {
        let out = km.load(LoadReq::new(id)).await.unwrap();
        assert_eq!(out["loaded"][0]["attach"], "attached", "{id}");
        let r = call(&hub, "km.specialist.call", json!({"id": id, "input": input}))
            .await
            .unwrap();
        assert_eq!(r["km"], format!("{id}@1.0.0"));
        assert_eq!(r["engine"], engine);
        assert!(!r["result"][field].is_null(), "{id}: {r}");
        // the KM's own model is what the engine is asked for
        let (seen_engine, args) = stub.seen.lock().last().cloned().unwrap();
        assert_eq!(seen_engine, engine);
        assert_eq!(args["model"], r["model"]);
        let tr = call_trace(&hub, r["trace"].as_str().unwrap());
        assert_eq!(tr["kind"], "km.specialist", "{tr}");
        assert_eq!(tr["status"], "ok");
        let c = &tr["specialist_calls"][0];
        assert_eq!(c["specialist"], format!("{id}@1.0.0"));
        assert_eq!(c["ok"], true);
        assert_eq!(c["input"], input);
        assert_eq!(c["output"], r["result"]);
    }
    assert_eq!(stub.seen.lock().len(), 4);
    assert_eq!(km.manager.refs_of("spec.stt@1.0.0"), 0, "the call's hold is released");
    // a call can be filed under a trace that exists
    let tr = crate::trace::tracer(&hub.engine.home);
    let own = tr.start("chat", "m", &json!({}), None, &json!({})).expect("a trace");
    let r = call(
        &hub,
        "km.specialist.call",
        json!({"id": "spec.stt", "input": {"file": "b.wav"}, "trace": own}),
    )
    .await
    .unwrap();
    assert_eq!(r["trace"], own.as_str());
    assert_eq!(call_trace(&hub, &own)["specialist_calls"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn specialist_calls_refuse_bad_input_unloaded_and_wrong_kms() {
    let (home, hub, km, _) = setup();
    spec_kms(&km, &home.join("src"));
    import(&km, &adapter(&home, "code.rust", "1.0.0", "rust"));
    let stub = Arc::new(StubEngines::default());
    km.set_specialist_backend(Some(stub.clone()));
    let ask = |id: &str, input: Value| call(&hub, "km.specialist.call", json!({"id": id, "input": input}));
    // not loaded yet, or not there at all: 404
    assert_eq!(
        ask("spec.tts", json!({"input": "hi", "output": "o"}))
            .await
            .unwrap_err()
            .status,
        404
    );
    assert_eq!(ask("spec.nope", json!({})).await.unwrap_err().status, 404);
    km.load(LoadReq::new("spec.tts")).await.unwrap();
    km.load(LoadReq::new("code.rust")).await.unwrap();
    // not a specialist, or input that breaks the schema: 400
    assert_eq!(ask("code.rust", json!({})).await.unwrap_err().status, 400);
    for bad in [
        json!({"input": "hi"}),
        json!({"input": 5, "output": "o"}),
        json!({"input": "x".repeat(21), "output": "o"}),
    ] {
        let e = ask("spec.tts", bad).await.unwrap_err();
        assert_eq!((e.status, e.code), (400, "invalid"), "{}", e.message);
        assert!(e.message.starts_with("input:"), "{}", e.message);
    }
    assert_eq!(ask("spec.tts", json!("text")).await.unwrap_err().status, 400);
    let e = call(&hub, "km.specialist.call", json!({"id": "spec.tts"}))
        .await
        .unwrap_err();
    assert_eq!(e.status, 400);
    assert!(stub.seen.lock().is_empty(), "nothing reached the engine");
    // an engine that fails: 503, and the call is recorded as failed
    let down = Arc::new(StubEngines {
        fail: true,
        ..Default::default()
    });
    km.set_specialist_backend(Some(down));
    let tr = crate::trace::tracer(&hub.engine.home);
    let own = tr.start("chat", "m", &json!({}), None, &json!({})).unwrap();
    let e = call(
        &hub,
        "km.specialist.call",
        json!({"id": "spec.tts", "input": {"input": "hi", "output": "o"}, "trace": own}),
    )
    .await
    .unwrap_err();
    assert_eq!((e.status, e.code), (503, "unavailable"));
    let c = call_trace(&hub, &own)["specialist_calls"][0].clone();
    assert_eq!(c["ok"], false);
    assert_eq!(c["output"]["code"], "unavailable");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn the_hub_backend_reports_an_engine_that_is_not_built_in_as_unavailable() {
    let (home, hub, km, _) = setup();
    // reachable as a hub operation, so also as a REST route and an MCP tool
    assert!(hub::operations().iter().any(|o| o["id"] == "km.specialist.call"));
    assert_eq!(ops::route_of("km.specialist.call"), "/v1/km/specialist/call");
    if !crate::vision::AVAILABLE {
        spec_kms(&km, &home.join("src"));
        km.load(LoadReq::new("spec.vis")).await.unwrap();
        let e = call(
            &hub,
            "km.specialist.call",
            json!({"id": "spec.vis", "input": {"image": "aGVsbG8="}}),
        )
        .await
        .unwrap_err();
        assert_eq!((e.status, e.code), (503, "unavailable"), "{}", e.message);
    }
    let _ = std::fs::remove_dir_all(&home);
}

// ---- the hub embedder drives KM search ------------------------------------------------------------------------

/// A stand-in `/v1/embeddings`: the texts that mention rust get one direction, sql another, the rest a third.
async fn embeddings_stub(seen: Arc<parking_lot::Mutex<Vec<Value>>>) -> (u16, tokio::task::JoinHandle<()>) {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/embeddings",
        post(move |axum::Json(req): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().push(req.clone());
                let texts: Vec<String> = match &req["input"] {
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(str::to_string).collect(),
                    Value::String(s) => vec![s.clone()],
                    _ => vec![],
                };
                let data: Vec<Value> = texts
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let v = if t.contains("rust") {
                            [1.0, 0.0, 0.0]
                        } else if t.contains("sql") {
                            [0.0, 1.0, 0.0]
                        } else {
                            [0.0, 0.0, 1.0]
                        };
                        json!({"object": "embedding", "index": i, "embedding": v})
                    })
                    .collect();
                axum::Json(json!({"object": "list", "model": req["model"], "data": data}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (
        port,
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        }),
    )
}

#[tokio::test]
async fn the_hub_embedder_ranks_km_search_with_the_vectors_the_hub_returns() {
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &["stub-embed"]);
    let km = runtime(&hub.engine).unwrap();
    let seen = Arc::new(parking_lot::Mutex::new(vec![]));
    let (port, server) = embeddings_stub(seen.clone()).await;
    let abort = server.abort_handle();
    hub.engine
        .adopt_as("stub-embed", port, "k", true, Some(Box::new(move || abort.abort())));
    settings(&hub, |s| {
        s.embedder = "hub".into();
        s.embed_model = Some("stub-embed".into());
    });
    let src = home.join("src");
    // the same words in both descriptors: only the centroids tell them apart
    for (id, centroid) in [("code.rust", [1.0f32, 0.0, 0.0]), ("code.sql", [0.0, 1.0, 0.0])] {
        let dir = Builder::new(&src, "adapter", id, "1.0.0")
            .descriptor("shared words")
            .embed_model("stub-embed")
            .centroid_vec(&centroid)
            .file("payload/a.gguf", "lora", "gguf", vec![3; 64])
            .build();
        import(&km, &dir);
    }
    for (query, first, second) in [
        ("shared rust", "code.rust", "code.sql"),
        ("shared sql", "code.sql", "code.rust"),
    ] {
        let r = call(&hub, "km.search", json!({"query": query})).await.unwrap();
        assert_eq!(r["embedder"], "stub-embed", "{r}");
        assert_eq!(r["results"][0]["id"], first, "{query}: {r}");
        assert_eq!(r["results"][1]["id"], second);
        assert_eq!(r["results"][0]["terms"]["vec"]["used"], true);
        let (a, b) = (
            r["results"][0]["terms"]["vec"]["value"].as_f64().unwrap(),
            r["results"][1]["terms"]["vec"]["value"].as_f64().unwrap(),
        );
        assert!((a - 1.0).abs() < 1e-3 && b.abs() < 1e-3, "{a} {b}");
    }
    let asked = seen.lock().clone();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0]["model"], "stub-embed");
    assert_eq!(asked[0]["input"][0], "shared rust");
    hub.engine.unload_all();
    let _ = std::fs::remove_dir_all(&home);
}

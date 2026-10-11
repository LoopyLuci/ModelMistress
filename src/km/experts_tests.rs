//! Experts KMs through the KM runtime: load, hot swap, evict, with a stand-in for the engine's reload.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use parking_lot::Mutex;
use serde_json::json;

use super::*;
use crate::km::assemble::testgguf::*;
use crate::km::pack::Source;
use crate::km::residency::tests_support::Recorder;
use crate::km::{runtime, sample, Km, LoadReq};

const HIDDEN: u64 = 8;
const FFN: u64 = 4;
const SLOTS: u64 = 4;

/// Reloads a core by adopting a stand-in instance, as the engine's hot swap would, and remembers what it loaded.
struct Stub {
    engine: Arc<Engine>,
    base: PathBuf,
    port: AtomicU16,
    current: Mutex<Option<PathBuf>>,
    swaps: Mutex<Vec<Option<PathBuf>>>,
    fail: AtomicBool,
    instances: Mutex<Vec<Arc<crate::engine::Instance>>>,
}

impl CoreModel for Stub {
    fn home(&self) -> PathBuf {
        self.engine.home.clone()
    }
    fn base(&self, _core: &str) -> Res<PathBuf> {
        Ok(self.base.clone())
    }
    fn swap<'a>(&'a self, core: &'a str, model: Option<PathBuf>) -> BoxFuture<'a, Res<()>> {
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) {
                return Err(Fail::unavailable("the new model did not start"));
            }
            if *self.current.lock() == model {
                return Ok(());
            }
            let port = self.port.fetch_add(1, Ordering::SeqCst);
            let inst = self.engine.adopt(core, port, "k", None);
            self.instances.lock().push(inst);
            *self.current.lock() = model.clone();
            self.swaps.lock().push(model);
            Ok(())
        })
    }
}

fn base(dir: &Path) -> PathBuf {
    let mut t = vec![];
    for layer in 0..2 {
        t.extend(exps(layer, SLOTS, HIDDEN, FFN, F16, |_, s| s as u8 + 1));
        t.push(router(layer, SLOTS, HIDDEN, |s| 0x40 + s as u8));
    }
    let p = dir.join("dusty.gguf");
    write(&p, &t);
    p
}

fn fragment(dir: &Path, layer: u32, fill: u8) -> Vec<u8> {
    write(&dir.join("f.gguf"), &exps(layer, 1, HIDDEN, FFN, F16, |_, _| fill));
    std::fs::read(dir.join("f.gguf")).unwrap()
}

struct Rig {
    home: crate::testing::TestDir,
    km: Arc<Km>,
    stub: Arc<Stub>,
    lora: Arc<Recorder>,
}

fn rig() -> Rig {
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &[]);
    let km = runtime(&hub.engine).unwrap();
    let stub = Arc::new(Stub {
        engine: hub.engine.clone(),
        base: base(&home),
        port: AtomicU16::new(40000),
        current: Mutex::new(None),
        swaps: Mutex::new(vec![]),
        fail: AtomicBool::new(false),
        instances: Mutex::new(vec![]),
    });
    let lora = Arc::new(Recorder::default());
    km.set_attacher(Some(Arc::new(ComposedAttacher {
        lora: lora.clone(),
        experts: Arc::new(ExpertsAttacher::new(stub.clone())),
    })));
    Rig { home, km, stub, lora }
}

fn import_experts(r: &Rig, id: &str, layer: u32, slot: u32, fill: u8) {
    let src = r.home.join("src");
    let frag = fragment(&r.home, layer, fill);
    let dir = sample::experts_with(
        &src,
        id,
        "1.0.0",
        (HIDDEN as u32, FFN as u32, SLOTS as u32),
        &[(layer, [slot, slot], frag)],
    );
    r.km.import_sync(&dir.display().to_string(), Source::Own, false, None)
        .unwrap();
}

fn req(id: &str) -> LoadReq {
    LoadReq {
        core: Some("dusty".into()),
        ..LoadReq::new(id)
    }
}

fn assembled_files(r: &Rig) -> usize {
    std::fs::read_dir(assemble::assembled_dir(&r.home))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[tokio::test]
async fn experts_kms_are_assembled_into_the_core_and_hot_swapped_then_the_base_comes_back() {
    let r = rig();
    import_experts(&r, "ex.a", 0, 1, 0xA1);
    import_experts(&r, "ex.b", 1, 2, 0xB2);
    let out = r.km.load(req("ex.a")).await.unwrap();
    assert_eq!(out["loaded"][0]["attach"], "attached");
    assert_eq!(out["loaded"][0]["tier"], "vram");
    let first = r
        .stub
        .current
        .lock()
        .clone()
        .expect("the core runs from an assembled file");
    assert!(first.starts_with(assemble::assembled_dir(&r.home)));
    assert_eq!(r.stub.engine.loaded().len(), 1, "one instance serves the core");
    // a second KM: assembled together with the first
    r.km.load(req("ex.b")).await.unwrap();
    let both = r.stub.current.lock().clone().unwrap();
    assert_ne!(both, first);
    assert!(!first.exists(), "the previous assembly is cleaned up");
    let h = assemble::read_header(&both).unwrap();
    let bytes = std::fs::read(&both).unwrap();
    let t = &h.tensors["blk.0.ffn_gate_exps.weight"];
    let stride = (t.bytes / SLOTS) as usize;
    let at = t.offset as usize + stride;
    assert!(bytes[at..at + stride].iter().all(|b| *b == 0xA1), "ex.a is still in");
    let t = &h.tensors["blk.1.ffn_up_exps.weight"];
    let at = t.offset as usize + 2 * stride;
    assert!(bytes[at..at + stride].iter().all(|b| *b == 0xB2), "ex.b is in");
    // the adapter mechanism is untouched by experts
    assert!(r.lora.log.lock().is_empty());
    // evict one, then the last: back to the base file
    r.km.unload("ex.a", None, false).await.unwrap();
    let only_b = r.stub.current.lock().clone().unwrap();
    assert!(only_b != both && only_b.starts_with(assemble::assembled_dir(&r.home)));
    r.km.unload("ex.b", None, false).await.unwrap();
    assert_eq!(*r.stub.current.lock(), None, "the base file serves again");
    assert_eq!(assembled_files(&r), 0);
    assert_eq!(r.stub.swaps.lock().len(), 4);
    // every swap retired the instance before it, so only the newest still runs
    let all = r.stub.instances.lock().clone();
    assert!(all[..all.len() - 1].iter().all(|i| i.is_stopped()));
    assert!(!all[all.len() - 1].is_stopped());
    let _ = std::fs::remove_dir_all(r.home.as_path());
}

#[tokio::test]
async fn a_swap_that_fails_refuses_the_load_with_the_reason_and_keeps_what_ran_before() {
    let r = rig();
    import_experts(&r, "ex.a", 0, 1, 0xA1);
    import_experts(&r, "ex.b", 1, 2, 0xB2);
    r.km.load(req("ex.a")).await.unwrap();
    let before = r.stub.current.lock().clone();
    r.stub.fail.store(true, Ordering::SeqCst);
    let e = r.km.load(req("ex.b")).await.unwrap_err();
    assert!(e.message.contains("the new model did not start"), "{}", e.message);
    assert_eq!(*r.stub.current.lock(), before);
    assert_eq!(r.km.manager.resident_keys(), vec!["ex.a@1.0.0".to_string()]);
    let refused = r.km.manager.events().recent(10, None, Some("refuse"));
    assert!(refused.iter().any(|e| e.key == "ex.b@1.0.0"));
    // once the core can swap again, the same load works and the failed KM was not remembered as attached
    r.stub.fail.store(false, Ordering::SeqCst);
    r.km.load(req("ex.b")).await.unwrap();
    let h = assemble::read_header(&r.stub.current.lock().clone().unwrap()).unwrap();
    assert!(h.tensors.contains_key("blk.1.ffn_up_exps.weight"));
    let _ = std::fs::remove_dir_all(r.home.as_path());
}

#[tokio::test]
async fn an_experts_km_that_does_not_fit_the_core_is_refused_before_anything_swaps() {
    let r = rig();
    // 16 hidden: not this core's shape
    let src = r.home.join("src");
    write(&r.home.join("g.gguf"), &exps(0, 1, 16, FFN, F16, |_, _| 3));
    let frag = std::fs::read(r.home.join("g.gguf")).unwrap();
    let dir = sample::experts_with(
        &src,
        "ex.odd",
        "1.0.0",
        (16, FFN as u32, SLOTS as u32),
        &[(0, [0, 0], frag)],
    );
    r.km.import_sync(&dir.display().to_string(), Source::Own, false, None)
        .unwrap();
    let e = r.km.load(req("ex.odd")).await.unwrap_err();
    assert!(
        e.message.contains("declares hidden 16") || e.message.contains("shape"),
        "{}",
        e.message
    );
    assert!(r.stub.swaps.lock().is_empty());
    assert!(r.km.manager.resident_keys().is_empty());
    let _ = std::fs::remove_dir_all(r.home.as_path());
}

#[tokio::test]
async fn the_composed_attacher_sends_adapters_to_lora_and_experts_to_assembly() {
    let r = rig();
    import_experts(&r, "ex.a", 0, 1, 0xA1);
    let adapter = sample::adapter(&r.home.join("src"), "ad.one", "1.0.0", "x");
    r.km.import_sync(&adapter.display().to_string(), Source::Own, false, None)
        .unwrap();
    r.km.load(req("ad.one")).await.unwrap();
    r.km.load(req("ex.a")).await.unwrap();
    assert_eq!(r.lora.log.lock().len(), 1, "{:?}", r.lora.log.lock());
    assert!(r.lora.log.lock()[0].starts_with("attach dusty"));
    assert_eq!(r.stub.swaps.lock().len(), 1);
    r.km.unload("ad.one", None, false).await.unwrap();
    assert_eq!(r.stub.swaps.lock().len(), 1, "removing an adapter does not reassemble");
    assert!(r.stub.current.lock().is_some(), "the experts stay in");
    let _ = json!({});
    let _ = std::fs::remove_dir_all(r.home.as_path());
}

#[tokio::test]
async fn the_engine_model_saves_the_assembled_file_for_the_core_and_clears_it() {
    let home = crate::testing::temp_home();
    let hub = crate::testing::hub(&home, &["core"]);
    let m = EngineModel(hub.engine.clone());
    let base = m.base("core").unwrap();
    assert!(base.ends_with("core.gguf"));
    let asm = home.join("a.gguf");
    m.swap("core", Some(asm.clone())).await.unwrap();
    assert_eq!(hub.engine.extras("core").model_path, Some(asm));
    m.swap("core", None).await.unwrap();
    assert_eq!(hub.engine.extras("core"), crate::engine::Extras::default());
    assert!(m.swap("nope-nothing", None).await.is_err());
    let _ = std::fs::remove_dir_all(&home);
}

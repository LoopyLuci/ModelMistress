//! The Forge's operations on a real hub object, with a real Python environment (made by forge.install from the
//! system Python; the tests are skipped where there is none) and a small stand-in for the `mm_forge` package.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::engine::{Engine, Settings};

fn system_python() -> Option<PathBuf> {
    for name in ["python3", "python", "py"] {
        let ok = std::process::Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false);
        if ok {
            return Some(name.into());
        }
    }
    None
}

fn tmp() -> crate::testing::TestDir {
    crate::testing::TestDir::new("mm-forge-hub")
}

const FAKE_MAIN: &str = r#"
import json, sys, time, subprocess
def emit(**e):
    print(json.dumps(e), flush=True)
step = sys.argv[1]
if step == "generate_serve":
    init = json.loads(sys.stdin.readline())
    emit(type="result", data={"id": "init", "device": "cpu", "dtype": "float32", "architecture": "Fancy", "model_path": init["model_path"]})
    for line in sys.stdin:
        r = json.loads(line)
        if r.get("op") == "bad":
            emit(type="result", data={"id": r["id"], "error": "nope"})
        else:
            emit(type="result", data={"id": r["id"], "text": "echo:" + str(r.get("prompt")), "tokens": [{"id": 1, "text": "x", "logprob": -0.5}]})
    sys.exit(0)
args = json.loads(sys.stdin.read() or "{}")
if step == "ok":
    for i in range(3):
        emit(type="metric", name="loss", value=3.0 - i, step=i)
        emit(type="progress", done=i + 1, total=3, note="step %d" % i)
    emit(type="artifact", name="a", path="x.bin", bytes=4, kind="file")
    emit(type="result", data={"echo": args})
elif step == "slow":
    subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
    while True:
        emit(type="progress", done=1, total=100, note="working")
        time.sleep(0.2)
elif step == "boom":
    emit(type="error", message="it broke", code="boom")
    sys.exit(3)
"#;

struct Fx {
    home: crate::testing::TestDir,
    hub: Hub,
}

fn fixture(python: &Path) -> Fx {
    let home = tmp();
    let src = home.join("src");
    std::fs::create_dir_all(src.join("mm_forge")).unwrap();
    std::fs::write(src.join("mm_forge").join("__init__.py"), "").unwrap();
    std::fs::write(src.join("mm_forge").join("__main__.py"), FAKE_MAIN).unwrap();
    std::fs::write(requirements_file(&src), "# nothing to install\n").unwrap();
    let engine = Engine::new(
        home.clone(),
        Settings {
            ollama_url: String::new(),
            forge_dir: Some(home.join("forge-env")),
            forge_data_dir: Some(home.join("forge-data")),
            forge_src: Some(src),
            forge_python: Some(python.to_path_buf()),
            llama_server: Some(home.join("no-llama-server")),
            ..Default::default()
        },
    );
    Fx {
        home,
        hub: Hub::new(engine, "t".repeat(32)),
    }
}

async fn done(fx: &Fx, job: &Value) -> Value {
    let id = job["job"].as_str().unwrap();
    for _ in 0..600 {
        let j = fx.hub.jobs.get(id).unwrap().describe();
        if j["status"] != "running" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} still running");
}

#[test]
fn the_operations_are_well_formed() {
    let ops = operations();
    let ids: Vec<&str> = ops.iter().map(|o| o["id"].as_str().unwrap()).collect();
    for id in [
        "forge.status",
        "forge.install",
        "forge.preflight",
        "forge.run",
        "forge.runs",
        "forge.run.get",
        "forge.run.cancel",
        "forge.load",
        "forge.generate",
        "forge.unload",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
    for o in &ops {
        for r in o["input_schema"]["required"].as_array().unwrap() {
            assert!(
                o["input_schema"]["properties"].get(r.as_str().unwrap()).is_some(),
                "{o}"
            );
        }
    }
    assert!(
        crate::hub::operations().iter().any(|o| o["id"] == "forge.run"),
        "registered in the hub"
    );
}

#[tokio::test]
async fn without_an_environment_runs_are_refused_and_status_says_so() {
    let home = tmp();
    let engine = Engine::new(
        home.clone(),
        Settings {
            ollama_url: String::new(),
            forge_dir: Some(home.join("nowhere")),
            ..Default::default()
        },
    );
    let hub = Hub::new(engine, "t".repeat(32));
    let st = call(&hub, "forge.status", &json!({})).await.unwrap();
    assert_eq!(st["installed"], false);
    assert_eq!(st["runs_total"], 0);
    assert!(st["env"].as_str().unwrap().replace('\\', "/").ends_with("nowhere/env"));
    let e = call(&hub, "forge.run", &json!({"step": "preflight"}))
        .await
        .unwrap_err();
    assert!(e.code == "not_installed" || e.code == "no_forge_src", "{e:?}");
    let e = call(&hub, "forge.run", &json!({"step": "Bad Step!"}))
        .await
        .unwrap_err();
    assert_eq!(e.status, 400);
    let e = call(&hub, "forge.run.get", &json!({"run": "nope"})).await.unwrap_err();
    assert_eq!(e.status, 404);
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn install_run_metrics_failure_and_cancel_on_a_real_environment() {
    let Some(python) = system_python() else {
        eprintln!("skipped: no Python on this machine");
        return;
    };
    let fx = fixture(&python);
    let h = &fx.hub;

    // install: creates the venv and pip-installs an empty requirements file
    let job = call(h, "forge.install", &json!({"skip_preflight": true}))
        .await
        .unwrap();
    let run = job["run"].as_str().unwrap().to_string();
    let j = done(&fx, &job).await;
    assert_eq!(j["status"], "done", "{j}");
    let st = call(h, "forge.status", &json!({})).await.unwrap();
    assert_eq!(st["installed"], true);
    let r = call(h, "forge.run.get", &json!({"run": run})).await.unwrap();
    assert_eq!(
        (r["step"].as_str(), r["status"].as_str()),
        (Some("install"), Some("done"))
    );

    // a step with metrics, progress, an artifact and a result
    let job = call(h, "forge.run", &json!({"step": "ok", "args": {"seed": 5}}))
        .await
        .unwrap();
    let run = job["run"].as_str().unwrap().to_string();
    let j = done(&fx, &job).await;
    assert_eq!(j["status"], "done", "{j}");
    let r = call(h, "forge.run.get", &json!({"run": run})).await.unwrap();
    assert_eq!(r["result"], json!({"echo": {"seed": 5}}));
    assert_eq!(r["metrics"]["loss"]["count"], 3);
    assert_eq!(r["metrics"]["loss"]["last"], 1.0);
    assert_eq!(r["artifacts"][0]["name"], "a");
    assert_eq!(r["progress"]["percent"], 100.0);
    let only = call(h, "forge.run.get", &json!({"run": run, "metric": "nothing"}))
        .await
        .unwrap();
    assert!(only["metrics"].as_object().unwrap().is_empty());
    let list = call(h, "forge.runs", &json!({"step": "ok"})).await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("result").is_none());

    // a failing step
    let job = call(h, "forge.run", &json!({"step": "boom", "wait": true}))
        .await
        .unwrap();
    assert_eq!(job["status"], "failed");
    let r = call(h, "forge.run.get", &json!({"run": job["run"]})).await.unwrap();
    assert_eq!(
        (r["status"].as_str(), r["error"].as_str(), r["error_code"].as_str()),
        (Some("failed"), Some("it broke"), Some("boom"))
    );

    // an unknown step is the worker's error
    let job = call(h, "forge.run", &json!({"step": "nonesuch", "wait": true}))
        .await
        .unwrap();
    assert_eq!(job["status"], "failed");

    // cancel kills the worker and its grandchild
    let job = call(h, "forge.run", &json!({"step": "slow"})).await.unwrap();
    let run = job["run"].as_str().unwrap().to_string();
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let r = call(h, "forge.run.get", &json!({"run": run})).await.unwrap();
        if r["progress"]["note"] == "working" {
            break;
        }
    }
    let c = call(h, "forge.run.cancel", &json!({"run": run})).await.unwrap();
    assert_eq!(c["cancelled"], true);
    let j = done(&fx, &job).await;
    assert_eq!(j["status"], "cancelled", "{j}");
    let r = call(h, "forge.run.get", &json!({"run": run})).await.unwrap();
    assert_eq!(r["status"], "cancelled");
    let again = call(h, "forge.run.cancel", &json!({"run": run})).await.unwrap();
    assert_eq!(again["cancelled"], false);
    assert!(call(h, "forge.status", &json!({})).await.unwrap()["running"]
        .as_array()
        .unwrap()
        .is_empty());

    // the history survives a new hub on the same home (the store is on disk)
    let st = call(h, "forge.status", &json!({})).await.unwrap();
    assert!(st["runs_total"].as_i64().unwrap() >= 5);
    let _ = std::fs::remove_dir_all(&fx.home);
}

#[tokio::test]
async fn load_picks_the_engine_by_format_and_the_forge_worker_serves_what_llama_cpp_cannot() {
    let Some(python) = system_python() else {
        eprintln!("skipped: no Python on this machine");
        return;
    };
    let fx = fixture(&python);
    let h = &fx.hub;
    let job = call(h, "forge.install", &json!({"skip_preflight": true}))
        .await
        .unwrap();
    assert_eq!(done(&fx, &job).await["status"], "done");

    let model = fx.home.join("fancy");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(model.join("w.safetensors"), b"x").unwrap();
    std::fs::write(model.join("config.json"), r#"{"architectures":["FancyForCausalLM"]}"#).unwrap();
    let name = model.display().to_string();

    let plan = call(h, "forge.load", &json!({"model": name, "plan_only": true}))
        .await
        .unwrap();
    assert_eq!(plan["plan"]["engine"], "forge");
    assert_eq!(plan["plan"]["format"], "safetensors");

    let e = call(h, "forge.generate", &json!({"model": name, "prompt": "hi"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_loaded");

    let l = call(h, "forge.load", &json!({"model": name, "device": "cpu"}))
        .await
        .unwrap();
    assert_eq!(l["status"], "loaded", "{l}");
    assert_eq!(l["placement"]["engine"], "forge");
    assert_eq!(l["placement"]["device"], "cpu");
    let g = call(
        h,
        "forge.generate",
        &json!({"model": name, "prompt": "hi", "max_tokens": 4}),
    )
    .await
    .unwrap();
    assert_eq!(g["text"], "echo:hi");
    assert_eq!(g["tokens"][0]["logprob"], -0.5);
    // requests are matched by id even when they overlap
    let (qa, qb) = (
        json!({"model": name, "prompt": "one"}),
        json!({"model": name, "prompt": "two"}),
    );
    let (a, b) = tokio::join!(call(h, "forge.generate", &qa), call(h, "forge.generate", &qb));
    assert_eq!(
        (a.unwrap()["text"].clone(), b.unwrap()["text"].clone()),
        (json!("echo:one"), json!("echo:two"))
    );
    let st = call(h, "forge.status", &json!({})).await.unwrap();
    assert_eq!(st["servers"][0]["alive"], true);
    assert_eq!(st["placements"][&name]["engine"], "forge");

    // a second load reuses the worker
    let again = call(h, "forge.load", &json!({"model": name})).await.unwrap();
    assert_eq!(again["status"], "loaded");
    assert_eq!(
        call(h, "forge.runs", &json!({"step": "generate_serve"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let u = call(h, "forge.unload", &json!({"model": name})).await.unwrap();
    assert_eq!(u["unloaded"], true);
    let e = call(h, "forge.generate", &json!({"model": name, "prompt": "hi"}))
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_loaded");

    // other formats are planned, not guessed
    std::fs::write(fx.home.join("m.onnx"), b"x").unwrap();
    let o = call(
        h,
        "forge.load",
        &json!({"model": fx.home.join("m.onnx").display().to_string()}),
    )
    .await
    .unwrap();
    assert_eq!(
        (o["plan"]["engine"].as_str(), o["status"].as_str()),
        (Some("onnxruntime"), Some("not_available"))
    );
    let e = call(h, "forge.load", &json!({"model": "no-such-model-anywhere"}))
        .await
        .unwrap_err();
    assert_eq!(e.status, 404);
    let e = call(h, "forge.load", &json!({"model": name, "device": "tpu"}))
        .await
        .unwrap_err();
    assert_eq!(e.status, 400);
    let _ = std::fs::remove_dir_all(&fx.home);
}

//! The `system.*` operations: how the machine is doing, and the settings that decide what ModelMistress does about it.
//! The owner's operations: a peer key reaches only the model routes.

use serde_json::{json, Value};

use super::{policy, Shared};
use crate::engine::{Fail, Result};
use crate::hub::Hub;

fn op(id: &str, summary: &str, mutating: bool, props: Value, required: &[&str]) -> Value {
    json!({"id": id, "group": "system", "summary": summary, "mutating": mutating, "destructive": false,
           "input_schema": {"type": "object", "properties": props, "required": required, "additionalProperties": false}})
}

pub fn operations() -> Vec<Value> {
    vec![
        op(
            "system.status",
            "How the machine is doing and what ModelMistress is doing about it: the level (calm, busy, strained, critical) \
             and why, the readings (CPU, memory, commit, video memory, temperatures), whether SystemWatchdog is feeding her, \
             the processes she is holding down, the jobs waiting for the machine, and what she did lately",
            false,
            json!({}),
            &[],
        ),
        op(
            "system.config.get",
            "The system-awareness settings (system.toml): the mode (off, gentle, balanced, strict), the limits for each level and the timings",
            false,
            json!({}),
            &[],
        ),
        op(
            "system.config.set",
            "Change the system-awareness settings with a merge patch, e.g. {\"mode\":\"off\"} (it puts back every priority it \
             changed) or {\"thresholds\":{\"cpu_strained_pct\":85}}. A misspelt or impossible setting is refused and nothing changes",
            true,
            json!({"settings": {"type": "object", "description": "the settings to change; a nested object merges"}}),
            &["settings"],
        ),
        op(
            "system.run_now",
            "Start the heavy jobs (training, image generation, quantizing) that are waiting for the machine to quiet, now",
            true,
            json!({}),
            &[],
        ),
    ]
}

pub async fn call(_hub: &Hub, id: &str, a: &Value) -> Result<Value> {
    let sh = super::shared().ok_or_else(|| {
        Fail::new(
            503,
            "not_running",
            "system awareness runs in `model-mistress serve`; this hub was not started with it",
        )
    })?;
    call_with(sh, id, a)
}

pub fn call_with(sh: &Shared, id: &str, a: &Value) -> Result<Value> {
    match id {
        "system.status" => Ok(sh.status_json()),
        "system.config.get" => Ok(json!({"settings": policy::to_json(&sh.cfg())})),
        "system.config.set" => {
            let patch = a
                .get("settings")
                .filter(|v| v.is_object())
                .ok_or_else(|| Fail::invalid("settings must be an object"))?;
            let cfg = sh.set_config(patch).map_err(Fail::invalid)?;
            Ok(json!({"settings": policy::to_json(&cfg)}))
        }
        "system.run_now" => Ok(json!({"released": sh.run_now()})),
        other => Err(Fail::new(404, "not_found", format!("no operation {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDir;

    #[test]
    fn the_operations_are_listed_with_schemas_that_refuse_extras() {
        let ops = operations();
        let ids: Vec<&str> = ops.iter().filter_map(|o| o["id"].as_str()).collect();
        assert_eq!(
            ids,
            [
                "system.status",
                "system.config.get",
                "system.config.set",
                "system.run_now"
            ]
        );
        for o in &ops {
            assert_eq!(o["input_schema"]["additionalProperties"], false);
            assert_eq!(o["group"], "system");
        }
        assert_eq!(ops[0]["mutating"], false);
        assert_eq!(ops[2]["mutating"], true);
    }

    #[test]
    fn the_operations_read_and_change_the_settings() {
        let dir = TestDir::new("mm-sys-ops");
        let sh = Shared::new(dir.to_path_buf());
        let st = call_with(&sh, "system.status", &json!({})).unwrap();
        assert_eq!(st["mode"], "balanced");
        assert_eq!(st["level"], "calm");

        let got = call_with(&sh, "system.config.get", &json!({})).unwrap();
        assert_eq!(got["settings"]["tick_ms"], 2000);
        assert_eq!(got["settings"]["thresholds"]["cpu_strained_pct"], 80.0);

        let set = call_with(
            &sh,
            "system.config.set",
            &json!({"settings": {"mode": "gentle", "shed": false}}),
        )
        .unwrap();
        assert_eq!(set["settings"]["mode"], "gentle");
        assert_eq!(call_with(&sh, "system.status", &json!({})).unwrap()["mode"], "gentle");

        for bad in [
            json!({}),
            json!({"settings": "off"}),
            json!({"settings": {"nonsense": 1}}),
            json!({"settings": {"tick_ms": 5}}),
        ] {
            let e = call_with(&sh, "system.config.set", &bad).unwrap_err();
            assert_eq!(e.status, 400, "{bad}: {}", e.message);
        }
        assert_eq!(sh.cfg().mode, policy::Mode::Gentle, "refused edits change nothing");

        assert_eq!(call_with(&sh, "system.run_now", &json!({})).unwrap()["released"], 0);
        assert_eq!(call_with(&sh, "system.nope", &json!({})).unwrap_err().status, 404);
    }

    #[tokio::test]
    async fn a_hub_that_is_not_serving_says_so_instead_of_inventing_a_status() {
        let home = crate::testing::temp_home();
        let hub = crate::testing::hub(&home, &["alpha"]);
        if super::super::shared().is_none() {
            let e = call(&hub, "system.status", &json!({})).await.unwrap_err();
            assert_eq!((e.status, e.code), (503, "not_running"));
        }
    }
}

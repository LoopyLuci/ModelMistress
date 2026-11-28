//! `model-mistress system`: how the machine is doing and what ModelMistress is doing about it.
//!
//! It asks the running hub. Settings can be changed without one: they are a file (`system.toml`), which the hub takes in
//! when it changes.

use std::path::Path;

use serde_json::{json, Value};

use super::policy::{self, Config, Mode};

pub const HELP: &str = "\
model-mistress system [command]

  (none) | status [--json]     the machine's level and why, what is being held down, the jobs waiting
  watch                        the same, every two seconds
  mode off|gentle|balanced|strict
                               off: change nothing and put back what was changed. gentle: lower background work only.
                               balanced (default): background work yields at once, model servers when the machine is
                               strained. strict: everything yields one level earlier
  config [key=value ...]       show the settings, or change some (values as JSON: thresholds.cpu_strained_pct=85)
  run-now                      start the heavy jobs that are waiting for the machine to quiet
";

fn secs(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn mib(v: &Value) -> Option<String> {
    v.as_u64().map(|m| {
        if m >= 10_240 {
            format!("{:.1} GiB", m as f64 / 1024.0)
        } else {
            format!("{m} MiB")
        }
    })
}

/// `system.status` as text.
pub fn render(st: &Value) -> String {
    let mut out = String::new();
    let level = st["level"].as_str().unwrap_or("?");
    let wd = &st["watchdog"];
    let wd_text = match wd["state"].as_str() {
        Some("connected") => format!(
            "connected (v{}, {:.1}s old)",
            wd["version"].as_str().unwrap_or("?"),
            wd["age_s"].as_f64().unwrap_or(0.0)
        ),
        Some("stale") => format!("answering but stale ({:.0}s old)", wd["age_s"].as_f64().unwrap_or(0.0)),
        Some("off") => "off (watchdog_url is empty)".to_string(),
        _ => "not found (the hub judges on what it measures itself)".to_string(),
    };
    out += &format!(
        "the machine is {level}{}   mode: {}\nwatchdog: {wd_text}\n",
        st["for_s"]
            .as_u64()
            .map(|s| format!(" (for {})", secs(s)))
            .unwrap_or_default(),
        st["mode"].as_str().unwrap_or("?")
    );
    if st["raw_level"] != st["level"] {
        out += &format!(
            "this look: {} (the level changes after a few looks in a row)\n",
            st["raw_level"].as_str().unwrap_or("?")
        );
    }
    let g = &st["signals"];
    let mut parts: Vec<String> = vec![];
    if let (Some(t), Some(m)) = (num(&g["cpu_total_pct"]), num(&g["cpu_mm_pct"])) {
        parts.push(format!(
            "cpu {t:.0}% (ModelMistress {m:.0}%, others {:.0}%)",
            (t - m).max(0.0)
        ));
    }
    if let (Some(a), Some(t)) = (
        g["ram_avail_mib"].as_u64(),
        g["ram_total_mib"].as_u64().filter(|t| *t > 0),
    ) {
        parts.push(format!(
            "memory {:.0}% free ({})",
            a as f64 * 100.0 / t as f64,
            mib(&g["ram_avail_mib"]).unwrap_or_default()
        ));
    }
    if let Some(c) = mib(&g["commit_headroom_mib"]) {
        parts.push(format!("commit {c} left"));
    }
    if let Some(v) = mib(&g["vram_free_mib"]) {
        parts.push(format!("video memory {v} free"));
    }
    if let Some(u) = num(&g["gpu_util_pct"]) {
        parts.push(format!("gpu {u:.0}%"));
    }
    if let Some(t) = num(&g["cpu_temp_c"]) {
        parts.push(format!("cpu {t:.0} C"));
    }
    if let Some(t) = num(&g["gpu_temp_c"]) {
        parts.push(format!("gpu {t:.0} C"));
    }
    if let Some(f) = g["foreground"].as_str() {
        parts.push(format!("in front: {f}"));
    }
    if !parts.is_empty() {
        out += &format!("  {}\n", parts.join("   "));
    }
    match st["reasons"].as_array().filter(|r| !r.is_empty()) {
        Some(rs) => {
            out += "why:\n";
            for r in rs {
                out += &format!(
                    "  [{}] {}\n",
                    r["level"].as_str().unwrap_or("?"),
                    r["detail"].as_str().unwrap_or("")
                );
            }
        }
        None => out += "why: nothing is pressing\n",
    }
    let held = st["holding"].as_array().cloned().unwrap_or_default();
    if held.is_empty() {
        out += "holding down: nothing\n";
    } else {
        out += &format!("holding down {}:\n", held.len());
        for h in &held {
            let mut what = vec![];
            if let (Some(f), Some(t)) = (h["from"].as_str(), h["to"].as_str()) {
                what.push(format!("{f} -> {t}"));
            }
            if h["efficiency_mode"] == true {
                what.push("efficiency mode".into());
            }
            if let Some(p) = h["paused_s"].as_u64() {
                what.push(format!("paused for {}", secs(p)));
            }
            out += &format!(
                "  {} (pid {}): {}\n",
                h["name"].as_str().unwrap_or("?"),
                h["pid"],
                what.join(", ")
            );
        }
    }
    if let Some(w) = st["waiting_jobs"].as_array().filter(|w| !w.is_empty()) {
        out += "waiting for the machine (`system run-now` starts them):\n";
        for j in w {
            out += &format!(
                "  {} {} \"{}\" for {}\n",
                j["kind"].as_str().unwrap_or("?"),
                j["job"].as_str().unwrap_or("?"),
                j["label"].as_str().unwrap_or(""),
                secs(j["waiting_s"].as_u64().unwrap_or(0))
            );
        }
    }
    if let Some(e) = st["recent"].as_array().filter(|e| !e.is_empty()) {
        out += "lately:\n";
        for ev in e.iter().take(8) {
            out += &format!(
                "  {} {}: {}\n",
                ev["name"].as_str().unwrap_or("?"),
                ev["pid"]
                    .as_u64()
                    .filter(|p| *p > 0)
                    .map(|p| format!("(pid {p})"))
                    .unwrap_or_default(),
                ev["what"].as_str().unwrap_or("")
            );
        }
    }
    if st["can_change_processes"] == false {
        out += "note: this platform lets the hub look and wait, but not change other processes' priorities\n";
    }
    if let Some(e) = st["settings_error"].as_str() {
        out += &format!("settings: {e}; the last good settings are in use\n");
    }
    out
}

/// `key=value` pairs into a merge patch: `thresholds.cpu_busy_pct=70` becomes a nested object. A value is JSON when it
/// parses as JSON, else a string (`mode=off`).
pub fn patch_of(pairs: &[String]) -> Result<Value, String> {
    let mut root = json!({});
    for p in pairs {
        let (k, v) = p.split_once('=').ok_or_else(|| format!("{p}: expected key=value"))?;
        let v: Value = serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string()));
        let mut at = &mut root;
        let mut keys = k.split('.').peekable();
        while let Some(key) = keys.next() {
            if key.is_empty() {
                return Err(format!("{p}: empty key"));
            }
            if keys.peek().is_none() {
                at[key] = v.clone();
            } else {
                at = &mut at[key];
                if !at.is_object() {
                    *at = json!({});
                }
            }
        }
    }
    Ok(root)
}

pub async fn run(home: &Path, args: Vec<String>) -> anyhow::Result<()> {
    let hub = crate::client::Client::find(Some(home.to_path_buf())).await.ok();
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    let rest = &args[args.len().min(1)..];
    match cmd {
        "help" | "-h" | "--help" => print!("{HELP}"),
        "status" => {
            let Some(c) = hub else {
                anyhow::bail!("no hub is running; system awareness lives in `model-mistress serve` (`system config` works without one)")
            };
            let st = c.call("system.status", json!({})).await?;
            if rest.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                print!("{}", render(&st));
            }
        }
        "watch" => {
            let Some(c) = hub else {
                anyhow::bail!("no hub is running")
            };
            loop {
                let st = c.call("system.status", json!({})).await?;
                print!("\x1b[2J\x1b[H{}", render(&st));
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
        "run-now" | "run_now" => {
            let Some(c) = hub else {
                anyhow::bail!("no hub is running")
            };
            let r = c.call("system.run_now", json!({})).await?;
            println!("{} job(s) were waiting and start now", r["released"]);
        }
        "mode" => {
            let m = rest.first().map(String::as_str).unwrap_or("");
            if Mode::parse(m).is_none() {
                anyhow::bail!("mode is one of off, gentle, balanced, strict");
            }
            set(home, hub, json!({"mode": m})).await?;
            println!("mode: {m}");
        }
        "config" => {
            if rest.is_empty() {
                let cfg = match &hub {
                    Some(c) => c.call("system.config.get", json!({})).await?["settings"].clone(),
                    None => policy::to_json(&Config::load(home).map_err(anyhow::Error::msg)?),
                };
                println!("{}", serde_json::to_string_pretty(&cfg)?);
            } else {
                let patch = patch_of(rest).map_err(anyhow::Error::msg)?;
                let new = set(home, hub, patch).await?;
                println!("{}", serde_json::to_string_pretty(&new)?);
            }
        }
        other => anyhow::bail!("unknown command {other}\n{HELP}"),
    }
    Ok(())
}

/// Applies a patch through the hub, or to the file when there is none (the hub takes it in when it next looks).
async fn set(home: &Path, hub: Option<crate::client::Client>, patch: Value) -> anyhow::Result<Value> {
    match hub {
        Some(c) => Ok(c.call("system.config.set", json!({"settings": patch})).await?["settings"].clone()),
        None => {
            let cfg = Config::load(home)
                .map_err(anyhow::Error::msg)?
                .patched(&patch)
                .map_err(anyhow::Error::msg)?;
            cfg.save(home).map_err(anyhow::Error::msg)?;
            Ok(policy::to_json(&cfg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_value_pairs_become_a_nested_patch() {
        let p = patch_of(&[
            "mode=off".into(),
            "thresholds.cpu_busy_pct=70".into(),
            "shed=false".into(),
            "thresholds.gpu_temp_critical_c=99.5".into(),
        ])
        .unwrap();
        assert_eq!(
            p,
            json!({"mode": "off", "shed": false, "thresholds": {"cpu_busy_pct": 70, "gpu_temp_critical_c": 99.5}})
        );
        assert!(patch_of(&["nonsense".into()]).is_err());
        assert!(patch_of(&["a..b=1".into()]).is_err());
    }

    #[test]
    fn settings_without_a_hub_go_to_the_file_and_refuse_what_is_wrong() {
        let dir = crate::testing::TestDir::new("mm-sys-cli");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            run(&dir, vec!["mode".into(), "strict".into()]).await.unwrap();
            assert_eq!(Config::load(&dir).unwrap().mode, Mode::Strict);
            run(
                &dir,
                vec![
                    "config".into(),
                    "thresholds.cpu_busy_pct=55".into(),
                    "shed=false".into(),
                ],
            )
            .await
            .unwrap();
            let c = Config::load(&dir).unwrap();
            assert_eq!((c.thresholds.cpu_busy_pct, c.shed), (55.0, false));
            assert!(run(&dir, vec!["mode".into(), "loud".into()]).await.is_err());
            assert!(
                run(&dir, vec!["config".into(), "thresholds.cpu_busy_pct=99".into()])
                    .await
                    .is_err(),
                "limits must stay in order"
            );
            assert_eq!(Config::load(&dir).unwrap().thresholds.cpu_busy_pct, 55.0);
            assert!(run(&dir, vec!["status".into()]).await.is_err(), "no hub: it says so");
        });
    }

    #[test]
    fn the_status_reads_as_a_few_lines() {
        let st = json!({
            "mode": "balanced", "level": "strained", "raw_level": "strained", "for_s": 125,
            "reasons": [{"resource": "cpu", "level": "strained", "detail": "other programs use 86% of the CPU (limit 80%), mostly game.exe (60%)"}],
            "signals": {"cpu_total_pct": 91.0, "cpu_mm_pct": 5.0, "ram_total_mib": 32768, "ram_avail_mib": 16384, "commit_headroom_mib": 30000,
                        "vram_free_mib": 4096, "gpu_util_pct": 97.0, "gpu_temp_c": 71.0, "foreground": "game.exe"},
            "watchdog": {"state": "connected", "version": "0.9.0", "age_s": 0.8},
            "holding": [{"pid": 1234, "name": "python.exe", "kind": "work", "from": "normal", "to": "idle", "efficiency_mode": true, "paused_s": null}],
            "waiting_jobs": [{"job": "forge-1", "kind": "forge", "label": "train", "waiting_s": 42}],
            "recent": [{"at": 1, "pid": 1234, "name": "python.exe", "what": "priority normal to idle"}],
            "can_change_processes": true, "settings_error": null
        });
        let t = render(&st);
        for want in [
            "the machine is strained (for 2m 5s)",
            "mode: balanced",
            "connected (v0.9.0, 0.8s old)",
            "cpu 91% (ModelMistress 5%, others 86%)",
            "memory 50% free",
            "video memory 4096 MiB free",
            "in front: game.exe",
            "[strained] other programs use 86%",
            "python.exe (pid 1234): normal -> idle, efficiency mode",
            "forge forge-1 \"train\" for 42s",
            "priority normal to idle",
        ] {
            assert!(t.contains(want), "missing {want:?} in:\n{t}");
        }
        // a quiet machine with no watchdog
        let quiet = render(
            &json!({"mode": "balanced", "level": "calm", "raw_level": "calm", "for_s": 5, "reasons": [], "signals": {},
                                   "watchdog": {"state": "absent"}, "holding": [], "waiting_jobs": [], "recent": [], "can_change_processes": true}),
        );
        assert!(
            quiet.contains("nothing is pressing")
                && quiet.contains("holding down: nothing")
                && quiet.contains("not found"),
            "{quiet}"
        );
    }
}

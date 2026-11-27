//! Watchdog's pressure feed, read from this side.
//!
//! SystemWatchdog (its own program) measures what the hub cannot by itself: the GPU's load and temperature, the CPU's
//! temperature, how busy the drives are, which program the user is working in, which programs are using the machine, and
//! whether the whole machine looks abnormal to its model. It publishes one small JSON document on a loopback port
//! (`GET /pressure`, read-only, no token). This module reads it.
//!
//! Watchdog is optional. When it is not running the hub still adapts on what it measures itself; this module then
//! says "absent", looks again every half minute, and never logs the failure more than once.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::level::Signals;

/// What the feed said, reduced to what the governor uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub age_s: f64,
    pub version: String,
    pub cpu_temp_c: Option<f64>,
    pub cpu_busiest_core_pct: Option<f64>,
    pub gpu_util_pct: Option<f64>,
    pub gpu_temp_c: Option<f64>,
    pub vram_total_mib: Option<u64>,
    pub vram_free_mib: Option<u64>,
    pub disk_busy_pct: Option<f64>,
    pub anomaly: Option<(f64, bool)>,
    pub foreground: Option<String>,
    pub on_battery: bool,
    /// (pid, name, cpu percent of the machine, gpu percent), busiest first.
    pub top: Vec<(u32, String, f64, f64)>,
}

/// Where the feed stands, for `system.status`.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// `watchdog_url` is empty.
    Off,
    /// Nothing answered.
    Absent,
    Connected {
        age_s: f64,
        version: String,
    },
    /// It answered, but its newest sample is older than `watchdog_max_age_s` (the monitor stopped or is paused).
    Stale {
        age_s: f64,
    },
}

impl State {
    pub fn to_json(&self, url: &str) -> Value {
        match self {
            State::Off => serde_json::json!({"state": "off", "url": null}),
            State::Absent => serde_json::json!({"state": "absent", "url": url}),
            State::Connected { age_s, version } => {
                serde_json::json!({"state": "connected", "url": url, "age_s": age_s, "version": version})
            }
            State::Stale { age_s } => serde_json::json!({"state": "stale", "url": url, "age_s": age_s}),
        }
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

/// The reading in a `/pressure` document, or None when it is not one (a different schema, or not an object).
/// `ncpu` turns Watchdog's per-process CPU (percent of one core) into a share of the machine.
pub fn parse(doc: &Value, ncpu: f64) -> Option<Reading> {
    if doc.get("schema")?.as_u64()? != 1 {
        return None;
    }
    let cpu = &doc["cpu"];
    // the card with the most memory is the one models run on; its figures stand for "the GPU"
    let gpu = doc["gpus"].as_array().and_then(|g| {
        g.iter().max_by(|a, b| {
            num(&a["mem_total_mb"])
                .unwrap_or(0.0)
                .partial_cmp(&num(&b["mem_total_mb"]).unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    let (total, used) = gpu
        .map(|g| (num(&g["mem_total_mb"]), num(&g["mem_used_mb"])))
        .unwrap_or((None, None));
    let top = doc["top"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|p| {
                    let pid = p["pid"].as_u64()? as u32;
                    Some((
                        pid,
                        p["name"].as_str().unwrap_or("?").to_string(),
                        num(&p["cpu_percent"]).unwrap_or(0.0) / ncpu.max(1.0),
                        num(&p["gpu_percent"]).unwrap_or(0.0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Reading {
        age_s: num(&doc["age_s"]).unwrap_or(0.0),
        version: doc["watchdog_version"].as_str().unwrap_or("").to_string(),
        cpu_temp_c: num(&cpu["temp_c"]),
        cpu_busiest_core_pct: num(&cpu["busiest_core_percent"]),
        gpu_util_pct: gpu.and_then(|g| num(&g["util_percent"])),
        gpu_temp_c: gpu.and_then(|g| num(&g["temp_c"])),
        vram_total_mib: total.filter(|t| *t > 0.0).map(|t| t as u64),
        vram_free_mib: match (total, used) {
            (Some(t), Some(u)) if t > 0.0 => Some((t - u).max(0.0) as u64),
            _ => None,
        },
        disk_busy_pct: num(&doc["disks"]["busiest_percent"]),
        anomaly: doc["anomaly"]
            .as_object()
            .and_then(|a| Some((a.get("score")?.as_f64()?, a.get("flag")?.as_bool().unwrap_or(false)))),
        foreground: doc["foreground"]["name"].as_str().map(str::to_string),
        on_battery: doc["battery"]["plugged"].as_bool() == Some(false),
        top,
    })
}

/// Puts a reading into the signals. `mine` are the pids of the hub and its descendants, which are not "other programs".
pub fn merge(s: &mut Signals, r: &Reading, mine: &dyn Fn(u32) -> bool) {
    s.watchdog = true;
    s.cpu_temp_c = r.cpu_temp_c.or(s.cpu_temp_c);
    s.cpu_busiest_core_pct = r.cpu_busiest_core_pct;
    s.gpu_util_pct = r.gpu_util_pct;
    s.gpu_temp_c = r.gpu_temp_c;
    s.disk_busy_pct = r.disk_busy_pct;
    s.anomaly = r.anomaly;
    s.foreground = r.foreground.clone();
    s.on_battery = r.on_battery;
    // Watchdog sees the whole card, other programs' use of it included; that is what the hub's own probe cannot see
    if r.vram_free_mib.is_some() {
        s.vram_total_mib = r.vram_total_mib;
        s.vram_free_mib = r.vram_free_mib;
    }
    let others: Vec<(String, f64, f64)> = r
        .top
        .iter()
        .filter(|(pid, ..)| !mine(*pid))
        .map(|(_, n, c, g)| (n.clone(), *c, *g))
        .take(3)
        .collect();
    if !others.is_empty() {
        s.offenders = others;
    }
}

pub struct Feed {
    http: reqwest::Client,
    state: State,
    next_try: Instant,
    misses: u32,
    ncpu: f64,
}

impl Feed {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(900))
            .build()
            .unwrap_or_default();
        Self {
            http,
            state: State::Absent,
            next_try: Instant::now(),
            misses: 0,
            ncpu: crate::machine::logical_cores() as f64,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// One look at the feed. Returns the reading when there is a fresh one. After misses it waits longer before it
    /// looks again (2 s, then 10 s, then 30 s), so a Watchdog that is not installed costs nothing.
    pub async fn poll(&mut self, url: &str, max_age_s: f64) -> Option<Reading> {
        if url.trim().is_empty() {
            self.state = State::Off;
            return None;
        }
        if Instant::now() < self.next_try {
            return None;
        }
        let doc = async {
            let r = self
                .http
                .get(format!("{}/pressure", url.trim_end_matches('/')))
                .send()
                .await
                .ok()?;
            if !r.status().is_success() {
                return None;
            }
            r.json::<Value>().await.ok()
        }
        .await;
        let reading = doc.and_then(|d| parse(&d, self.ncpu));
        match reading {
            Some(r) => {
                self.misses = 0;
                self.next_try = Instant::now();
                if r.age_s > max_age_s {
                    self.state = State::Stale { age_s: r.age_s };
                    None
                } else {
                    self.state = State::Connected {
                        age_s: r.age_s,
                        version: r.version.clone(),
                    };
                    Some(r)
                }
            }
            None => {
                self.misses = self.misses.saturating_add(1);
                self.state = State::Absent;
                let wait = match self.misses {
                    0..=2 => 2,
                    3..=5 => 10,
                    _ => 30,
                };
                self.next_try = Instant::now() + Duration::from_secs(wait);
                None
            }
        }
    }

    /// Look again at the next poll (the setting changed).
    pub fn retry_now(&mut self) {
        self.misses = 0;
        self.next_try = Instant::now();
    }
}

impl Default for Feed {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn doc() -> Value {
        json!({
            "schema": 1, "watchdog_version": "0.2.2", "age_s": 1.5,
            "cpu": {"percent": 40.0, "busiest_core_percent": 97.0, "temp_c": 71.0},
            "gpus": [
                {"index": 0, "name": "igpu", "util_percent": 3.0, "temp_c": 40.0, "mem_used_mb": 100.0, "mem_total_mb": 512.0},
                {"index": 1, "name": "RX 7900 XTX", "util_percent": 88.0, "temp_c": 77.0, "mem_used_mb": 20000.0, "mem_total_mb": 24560.0}
            ],
            "disks": {"busiest_percent": 55.0, "busiest_mount": "D:\\"},
            "battery": {"percent": 50.0, "plugged": false},
            "anomaly": {"score": 0.4, "flag": false, "source": "model"},
            "foreground": {"pid": 9, "name": "game.exe"},
            "top": [
                {"pid": 100, "name": "llama-server.exe", "cpu_percent": 480.0, "gpu_percent": 10.0},
                {"pid": 9, "name": "game.exe", "cpu_percent": 240.0, "gpu_percent": 85.0},
                {"pid": 8, "name": "chrome.exe", "cpu_percent": 48.0, "gpu_percent": 0.0}
            ]
        })
    }

    #[test]
    fn the_card_with_the_most_memory_stands_for_the_gpu() {
        let r = parse(&doc(), 24.0).unwrap();
        assert_eq!((r.gpu_util_pct, r.gpu_temp_c), (Some(88.0), Some(77.0)));
        assert_eq!((r.vram_total_mib, r.vram_free_mib), (Some(24560), Some(4560)));
        assert_eq!(
            (r.cpu_temp_c, r.cpu_busiest_core_pct, r.disk_busy_pct),
            (Some(71.0), Some(97.0), Some(55.0))
        );
        assert_eq!(r.anomaly, Some((0.4, false)));
        assert_eq!(r.foreground.as_deref(), Some("game.exe"));
        assert!(r.on_battery);
        assert_eq!(
            r.top[1],
            (9, "game.exe".into(), 10.0, 85.0),
            "cpu percent of one core becomes a share of the machine"
        );
    }

    #[test]
    fn a_document_of_another_schema_or_shape_is_not_read() {
        assert!(parse(&json!({"schema": 2}), 8.0).is_none());
        assert!(parse(&json!([1, 2]), 8.0).is_none());
        assert!(parse(&json!({}), 8.0).is_none());
        let r = parse(&json!({"schema": 1}), 8.0).unwrap();
        assert_eq!(
            r,
            Reading::default(),
            "a sparse document is a reading with nothing in it"
        );
    }

    #[test]
    fn merging_adds_what_the_hub_cannot_see_and_leaves_its_own_figures() {
        let r = parse(&doc(), 24.0).unwrap();
        let mut s = Signals {
            cpu_temp_c: Some(1.0),
            vram_free_mib: Some(99_999),
            ..Default::default()
        };
        merge(&mut s, &r, &|pid| pid == 100);
        assert!(s.watchdog);
        assert_eq!(
            s.vram_free_mib,
            Some(4560),
            "Watchdog sees other programs' video memory"
        );
        assert_eq!(s.cpu_temp_c, Some(71.0));
        assert_eq!(
            s.offenders.iter().map(|o| o.0.as_str()).collect::<Vec<_>>(),
            ["game.exe", "chrome.exe"],
            "our own server is not an offender"
        );
        let mut keep = Signals {
            vram_free_mib: Some(7),
            ..Default::default()
        };
        merge(&mut keep, &Reading::default(), &|_| false);
        assert_eq!(
            keep.vram_free_mib,
            Some(7),
            "no video memory figure from Watchdog: the hub's own stands"
        );
    }

    /// A one-request HTTP server that answers every request with `body`.
    async fn serve(body: String, status: &'static str) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let r = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = s.write_all(r.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_running_feed_is_read_and_a_stale_one_is_ignored() {
        let url = serve(doc().to_string(), "200 OK").await;
        let mut f = Feed::new();
        let r = f.poll(&url, 15.0).await.expect("a fresh reading");
        assert_eq!(r.gpu_util_pct, Some(88.0));
        assert_eq!(
            f.state(),
            &State::Connected {
                age_s: 1.5,
                version: "0.2.2".into()
            }
        );
        let mut old = doc();
        old["age_s"] = json!(400.0);
        let url = serve(old.to_string(), "200 OK").await;
        let mut g = Feed::new();
        assert!(g.poll(&url, 15.0).await.is_none());
        assert_eq!(g.state(), &State::Stale { age_s: 400.0 });
    }

    #[tokio::test]
    async fn a_missing_feed_is_absent_and_is_looked_for_less_and_less_often() {
        let mut f = Feed::new();
        assert!(f.poll("http://127.0.0.1:1", 15.0).await.is_none());
        assert_eq!(f.state(), &State::Absent);
        assert!(f.next_try > Instant::now(), "it waits before looking again");
        for _ in 0..8 {
            f.next_try = Instant::now();
            f.poll("http://127.0.0.1:1", 15.0).await;
        }
        assert!(
            f.next_try > Instant::now() + Duration::from_secs(20),
            "after many misses it looks every half minute"
        );
        f.retry_now();
        assert!(f.next_try <= Instant::now());
    }

    #[tokio::test]
    async fn an_error_answer_or_a_blank_url_is_not_a_reading() {
        let url = serve("{\"error\":\"no sample yet\"}".into(), "503 Service Unavailable").await;
        let mut f = Feed::new();
        assert!(f.poll(&url, 15.0).await.is_none());
        assert_eq!(f.state(), &State::Absent);
        assert!(f.poll("  ", 15.0).await.is_none());
        assert_eq!(f.state(), &State::Off);
    }
}

//! Batch jobs (docs/agents.md, item 4): a list of prompts runs at a set concurrency, and each answer is written to disk as it
//! finishes. A crash loses only the answers in progress: on startup the hub runs again the items that have no answer yet.
//!
//! A job is a folder `<home>/batches/<id>/`: `job.json` (what was asked, and the state; rewritten when the state changes)
//! and `results.jsonl` (one line per finished item, appended and synced). A line cut off by a crash is dropped on startup.
//! A failed item is an answer too: it is recorded with its error, and the rest of the job runs.

use crate::engine::Fail;
use crate::hub::{call_boxed, Hub};
use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Prompts one batch may hold.
pub const MAX_PROMPTS: usize = 10_000;
/// Items one job runs at once, at most.
pub const MAX_CONCURRENCY: usize = 8;
const DEFAULT_CONCURRENCY: usize = 2;
const DEFAULT_MAX_TOKENS: u64 = 512;
const MAX_TOKENS_LIMIT: u64 = 8192;
const DIR: &str = "batches";
const JOB: &str = "job.json";
const RESULTS: &str = "results.jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Done,
    Cancelled,
}

impl JobState {
    /// The state a `batch.list` filter names: `all` is no filter (None).
    pub fn filter(name: &str) -> Result<Option<JobState>, Fail> {
        match name {
            "running" => Ok(Some(JobState::Running)),
            "done" => Ok(Some(JobState::Done)),
            "cancelled" => Ok(Some(JobState::Cancelled)),
            "all" => Ok(None),
            other => Err(Fail::invalid(format!(
                "state is running, done, cancelled or all, not {other}"
            ))),
        }
    }
}

/// What a job asks, and its state. Written to `job.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: u64,
    pub concurrency: usize,
    pub prompts: Vec<String>,
    pub state: JobState,
    /// Unix seconds when the job was submitted, and when it stopped running.
    pub created: i64,
    pub finished: Option<i64>,
}

/// One finished item: a line of `results.jsonl`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemResult {
    /// The item's place in the job's prompts.
    pub index: usize,
    pub ok: bool,
    /// The answer, when `ok`.
    pub answer: Option<String>,
    /// The hub's code and message, when not `ok`.
    pub error: Option<String>,
    pub completion_tokens: u64,
}

/// A checked `batch.submit`.
#[derive(Debug, PartialEq)]
pub struct NewJob {
    pub name: String,
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: u64,
    pub concurrency: usize,
    pub prompts: Vec<String>,
}

/// Check what `batch.submit` was given. Nothing is written until every check passes.
pub fn parse_spec(a: &Value) -> Result<NewJob, Fail> {
    let model = a["model"].as_str().unwrap_or("").trim();
    if model.is_empty() {
        return Err(Fail::invalid("batch.submit needs \"model\": the name of a model"));
    }
    let list = a["prompts"]
        .as_array()
        .ok_or_else(|| Fail::invalid("batch.submit needs \"prompts\": a list of strings"))?;
    if list.is_empty() {
        return Err(Fail::invalid("the batch has no prompts"));
    }
    if list.len() > MAX_PROMPTS {
        return Err(Fail::invalid(format!(
            "a batch takes at most {MAX_PROMPTS} prompts, not {}",
            list.len()
        )));
    }
    let prompts = list
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_string)
                .ok_or_else(|| Fail::invalid("every prompt is a string"))
        })
        .collect::<Result<Vec<String>, Fail>>()?;
    let concurrency = a["concurrency"].as_u64().unwrap_or(DEFAULT_CONCURRENCY as u64);
    if !(1..=MAX_CONCURRENCY as u64).contains(&concurrency) {
        return Err(Fail::invalid(format!(
            "concurrency is 1 to {MAX_CONCURRENCY}, not {concurrency}"
        )));
    }
    let max_tokens = a["max_tokens"].as_u64().unwrap_or(DEFAULT_MAX_TOKENS);
    if !(1..=MAX_TOKENS_LIMIT).contains(&max_tokens) {
        return Err(Fail::invalid(format!(
            "max_tokens is 1 to {MAX_TOKENS_LIMIT}, not {max_tokens}"
        )));
    }
    Ok(NewJob {
        name: a["name"].as_str().unwrap_or("").trim().chars().take(80).collect(),
        model: model.to_string(),
        system: a["system"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        max_tokens,
        concurrency: concurrency as usize,
        prompts,
    })
}

/// The items that have no answer yet, in order.
pub fn pending(total: usize, done: &HashSet<usize>) -> Vec<usize> {
    (0..total).filter(|i| !done.contains(i)).collect()
}

/// The lines of a results file that parse. A line cut off by a crash does not.
fn read_results(path: &Path) -> Vec<ItemResult> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

/// As `read_results`, and when a line was cut off, the file is rewritten with the whole lines only, so the next answer
/// is not appended to the broken one.
fn compact(path: &Path) -> Vec<ItemResult> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let good = read_results(path);
    if good.len() != text.lines().count() {
        let body: String = good
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .map(|line| line + "\n")
            .collect();
        let tmp = path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
            eprintln!("batch results {} could not be tidied: {e}", path.display());
        }
    }
    good
}

fn append(path: &Path, r: &ItemResult) -> std::io::Result<()> {
    let mut line = serde_json::to_string(r).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.sync_data()
}

fn save_job(dir: &Path, job: &Job) -> std::io::Result<()> {
    let tmp = dir.join("job.json.tmp");
    let bytes = serde_json::to_vec_pretty(job).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(JOB))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn io_fail(e: std::io::Error) -> Fail {
    Fail::unavailable(format!("the batch could not be written: {e}"))
}

/// The batch folder under the hub's home, and the jobs that are being cancelled.
#[derive(Default)]
pub struct Batches {
    /// `<home>/batches`. None for a hub with no home (tests): no batch can be submitted.
    dir: Option<PathBuf>,
    cancelled: Mutex<HashSet<String>>,
}

impl Batches {
    pub fn open(home: &Path) -> Self {
        Self {
            dir: Some(home.join(DIR)),
            cancelled: Mutex::new(HashSet::new()),
        }
    }

    fn is_cancelled(&self, id: &str) -> bool {
        self.cancelled.lock().contains(id)
    }

    fn root(&self) -> Result<&Path, Fail> {
        self.dir
            .as_deref()
            .ok_or_else(|| Fail::unavailable("this hub has no home folder, so it keeps no batches"))
    }

    /// The folder of a job. An id is ours (letters and digits), so a request cannot name a path outside the batches.
    fn job_dir(&self, id: &str) -> Result<PathBuf, Fail> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Fail::not_found(format!("no batch {id:?} (batch.list shows them)")));
        }
        Ok(self.root()?.join(id))
    }

    fn load(&self, id: &str) -> Result<Job, Fail> {
        let dir = self.job_dir(id)?;
        let bytes = std::fs::read(dir.join(JOB))
            .map_err(|_| Fail::not_found(format!("no batch {id} (batch.list shows them)")))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Fail::unavailable(format!("batch {id}: job.json cannot be read: {e}")))
    }
}

/// The summary of a job for the owner: its progress, not its prompts.
fn view(job: &Job, done: usize, failed: usize) -> Value {
    json!({"id": job.id, "name": job.name, "model": job.model, "state": job.state, "total": job.prompts.len(),
           "done": done, "failed": failed, "concurrency": job.concurrency, "max_tokens": job.max_tokens,
           "created": job.created, "finished": job.finished})
}

/// The progress of a job, from its results file: (items finished, items that failed).
fn progress(dir: &Path) -> (usize, usize) {
    let results = read_results(&dir.join(RESULTS));
    (results.len(), results.iter().filter(|r| !r.ok).count())
}

/// `batch.submit`: check the job, write its folder, and start it. Returns at once, with the job's id.
pub fn submit(hub: &Hub, a: &Value) -> Result<Value, Fail> {
    let spec = parse_spec(a)?;
    let root = hub.batches.root()?.to_path_buf();
    let job = Job {
        id: format!("b{}", uuid::Uuid::new_v4().simple()),
        name: spec.name,
        model: spec.model,
        system: spec.system,
        max_tokens: spec.max_tokens,
        concurrency: spec.concurrency,
        prompts: spec.prompts,
        state: JobState::Running,
        created: now(),
        finished: None,
    };
    let dir = root.join(&job.id);
    std::fs::create_dir_all(&dir).map_err(io_fail)?;
    save_job(&dir, &job).map_err(io_fail)?;
    let shown = view(&job, 0, 0);
    spawn(hub.clone(), job, HashSet::new());
    Ok(shown)
}

/// `batch.get`: the job's state and progress.
pub fn get(hub: &Hub, id: &str) -> Result<Value, Fail> {
    let job = hub.batches.load(id)?;
    let (done, failed) = progress(&hub.batches.job_dir(id)?);
    Ok(view(&job, done, failed))
}

/// `batch.list`: the jobs in a state (all with `all`), newest first.
pub fn list(hub: &Hub, state: &str) -> Result<Value, Fail> {
    let want = JobState::filter(state)?;
    let root = hub.batches.root()?;
    let mut jobs: Vec<Job> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path().join(JOB)).ok())
        .filter_map(|bytes| serde_json::from_slice::<Job>(&bytes).ok())
        .filter(|job| want.is_none_or(|s| job.state == s))
        .collect();
    jobs.sort_by(|a, b| b.created.cmp(&a.created));
    let rows: Vec<Value> = jobs
        .iter()
        .map(|job| {
            let (done, failed) = progress(&root.join(&job.id));
            view(job, done, failed)
        })
        .collect();
    Ok(json!({"batches": rows}))
}

/// `batch.results`: the finished items, in the order they finished, from a position (`from`, at most `limit` of them).
/// `next` is the position to ask from next, or null when there are no more.
pub fn results(hub: &Hub, id: &str, from: usize, limit: usize) -> Result<Value, Fail> {
    hub.batches.load(id)?;
    let all = read_results(&hub.batches.job_dir(id)?.join(RESULTS));
    let end = all.len().min(from.saturating_add(limit.clamp(1, 1000)));
    let page: &[ItemResult] = if from < all.len() { &all[from..end] } else { &[] };
    let next = (end < all.len()).then_some(end);
    Ok(json!({"results": page, "next": next}))
}

/// `batch.cancel`: stop a running job. The items already running finish; no item after them starts.
pub fn cancel(hub: &Hub, id: &str) -> Result<Value, Fail> {
    let mut job = hub.batches.load(id)?;
    if job.state != JobState::Running {
        return Err(Fail::new(
            409,
            "not_running",
            format!("batch {id} is {:?}, not running", job.state),
        ));
    }
    let dir = hub.batches.job_dir(id)?;
    hub.batches.cancelled.lock().insert(id.to_string());
    job.state = JobState::Cancelled;
    job.finished = Some(now());
    save_job(&dir, &job).map_err(io_fail)?;
    let (done, failed) = progress(&dir);
    Ok(view(&job, done, failed))
}

/// On startup: every job that was running when the hub stopped runs again, for the items with no answer yet.
pub fn resume(hub: &Hub) {
    let Some(root) = hub.batches.dir.clone() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(bytes) = std::fs::read(dir.join(JOB)) else {
            continue;
        };
        let Ok(job) = serde_json::from_slice::<Job>(&bytes) else {
            eprintln!(
                "model-mistress: {} is not a readable batch; it is left as it is",
                dir.display()
            );
            continue;
        };
        if job.state != JobState::Running {
            continue;
        }
        let answered: HashSet<usize> = compact(&dir.join(RESULTS)).iter().map(|r| r.index).collect();
        eprintln!(
            "model-mistress: resuming batch {} ({} of {} answered)",
            job.id,
            answered.len(),
            job.prompts.len()
        );
        spawn(hub.clone(), job, answered);
    }
}

fn spawn(hub: Hub, job: Job, answered: HashSet<usize>) {
    tokio::spawn(run(hub, job, answered));
}

/// Run the items with no answer yet, `concurrency` at a time, and write each answer as it finishes.
async fn run(hub: Hub, job: Job, answered: HashSet<usize>) {
    let Some(root) = hub.batches.dir.clone() else {
        return;
    };
    let dir = root.join(&job.id);
    let batches = hub.batches.clone();
    let todo = pending(job.prompts.len(), &answered);
    let jobs = Arc::new(job.clone());
    let mut answers = futures::stream::iter(todo)
        .map(move |index| {
            let hub = hub.clone();
            let jobs = jobs.clone();
            async move {
                if hub.batches.is_cancelled(&jobs.id) {
                    return None;
                }
                Some(answer_one(&hub, &jobs, index).await)
            }
        })
        .buffer_unordered(job.concurrency);
    while let Some(answer) = answers.next().await {
        if let Some(r) = answer {
            if let Err(e) = append(&dir.join(RESULTS), &r) {
                eprintln!(
                    "batch {}: the answer to item {} could not be written: {e}",
                    job.id, r.index
                );
            }
        }
    }
    let mut finished = job.clone();
    finished.state = if batches.is_cancelled(&job.id) {
        JobState::Cancelled
    } else {
        JobState::Done
    };
    finished.finished = Some(now());
    if let Err(e) = save_job(&dir, &finished) {
        eprintln!("batch {}: its final state could not be written: {e}", job.id);
    }
}

/// One item: the prompt goes through `chat.complete`, as the owner's own chat does. A failure is recorded, not raised.
async fn answer_one(hub: &Hub, job: &Job, index: usize) -> ItemResult {
    let mut args = json!({"model": job.model, "prompt": job.prompts[index], "max_tokens": job.max_tokens});
    if let Some(system) = &job.system {
        args["system"] = json!(system);
    }
    match call_boxed(hub, "chat.complete", &args).await {
        Ok(v) => ItemResult {
            index,
            ok: true,
            // `chat.complete` answers with the text itself (`text`), and the usage of the reply
            answer: v["text"].as_str().map(str::to_string),
            error: None,
            completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        },
        Err(e) => {
            let f: Fail = e.into();
            ItemResult {
                index,
                ok: false,
                answer: None,
                error: Some(format!("{}: {}", f.code, f.message)),
                completion_tokens: 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(v: Value) -> Result<NewJob, Fail> {
        parse_spec(&v)
    }

    #[test]
    fn a_spec_is_checked_before_anything_is_written() {
        assert_eq!(spec(json!({"prompts": ["x"]})).unwrap_err().code, "invalid");
        assert!(spec(json!({"model": "m", "prompts": "not a list"})).is_err());
        assert!(spec(json!({"model": "m", "prompts": []})).is_err());
        assert!(spec(json!({"model": "m", "prompts": [1]})).is_err());
        let too_many: Vec<&str> = vec!["p"; MAX_PROMPTS + 1];
        assert!(spec(json!({"model": "m", "prompts": too_many})).is_err());
        assert!(spec(json!({"model": "m", "prompts": ["p"], "concurrency": 0})).is_err());
        assert!(spec(json!({"model": "m", "prompts": ["p"], "concurrency": MAX_CONCURRENCY + 1})).is_err());
        assert!(spec(json!({"model": "m", "prompts": ["p"], "max_tokens": 0})).is_err());
        let ok =
            spec(json!({"model": " m ", "prompts": ["a", "b"], "name": " nightly ", "system": " be brief "})).unwrap();
        assert_eq!(
            (
                ok.model.as_str(),
                ok.name.as_str(),
                ok.system.as_deref(),
                ok.concurrency,
                ok.max_tokens
            ),
            (
                "m",
                "nightly",
                Some("be brief"),
                DEFAULT_CONCURRENCY,
                DEFAULT_MAX_TOKENS
            )
        );
        assert_eq!(ok.prompts, ["a", "b"]);
        let long = spec(json!({"model": "m", "prompts": ["p"], "name": "n".repeat(200)})).unwrap();
        assert_eq!(long.name.chars().count(), 80);
    }

    #[test]
    fn pending_is_the_items_without_an_answer_in_order() {
        let answered: HashSet<usize> = [1, 3].into_iter().collect();
        assert_eq!(pending(5, &answered), vec![0, 2, 4]);
        assert!(pending(2, &[0, 1].into_iter().collect()).is_empty());
    }

    #[test]
    fn a_line_cut_off_by_a_crash_is_dropped_and_the_file_is_made_whole() {
        let dir = crate::testing::TestDir::new("mm-batch");
        let path = dir.join(RESULTS);
        let ok = |i| ItemResult {
            index: i,
            ok: true,
            answer: Some("a".into()),
            error: None,
            completion_tokens: 3,
        };
        let whole = format!(
            "{}\n{}\n{{\"index\":2,\"ok\":tr",
            serde_json::to_string(&ok(0)).unwrap(),
            serde_json::to_string(&ok(1)).unwrap()
        );
        std::fs::write(&path, whole).unwrap();
        let kept = compact(&path);
        assert_eq!(kept.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            2,
            "the broken line is gone"
        );
    }

    #[test]
    fn a_job_is_found_by_its_id_and_a_path_cannot_be_named() {
        let batches = Batches::open(Path::new("/home/mm"));
        assert!(batches.job_dir("../etc").is_err());
        assert!(batches.job_dir("").is_err());
        assert!(batches.job_dir("b0123abc").is_ok());
        assert_eq!(JobState::filter("running").unwrap(), Some(JobState::Running));
        assert_eq!(JobState::filter("all").unwrap(), None);
    }

    /// Poll a job until it stops running, for a minute at most.
    async fn settled(hub: &Hub, id: &str) -> Value {
        for _ in 0..600 {
            let v = get(hub, id).unwrap();
            if v["state"] != "running" {
                return v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("batch {id} did not stop running in a minute");
    }

    #[tokio::test]
    async fn a_batch_answers_every_item_and_a_failed_item_is_recorded_too() {
        let hub = crate::testing::hub(&crate::testing::temp_home(), &["alpha"]);
        let started = submit(
            &hub,
            &json!({"model": "alpha", "prompts": ["one", "two", "three"], "concurrency": 2}),
        )
        .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        assert_eq!(started["state"], "running", "{started}");
        let finished = settled(&hub, &id).await;
        assert_eq!(finished["state"], "done", "{finished}");
        assert_eq!(
            (finished["done"].as_u64(), finished["total"].as_u64()),
            (Some(3), Some(3)),
            "{finished}"
        );
        let page = results(&hub, &id, 0, 10).unwrap();
        let mut indexes: Vec<u64> = page["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["index"].as_u64().unwrap())
            .collect();
        indexes.sort();
        assert_eq!(indexes, vec![0, 1, 2], "every item has its answer, or its error");
        assert!(page["next"].is_null());
        assert_eq!(list(&hub, "done").unwrap()["batches"].as_array().unwrap().len(), 1);
        assert!(list(&hub, "running").unwrap()["batches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_finished_batch_cannot_be_cancelled_and_an_unknown_one_is_not_found() {
        let hub = crate::testing::hub(&crate::testing::temp_home(), &["alpha"]);
        let started = submit(&hub, &json!({"model": "alpha", "prompts": ["only"]})).unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        settled(&hub, &id).await;
        assert_eq!(cancel(&hub, &id).unwrap_err().code, "not_running");
        assert_eq!(get(&hub, "bnothere").unwrap_err().status, 404);
        assert_eq!(get(&hub, "../escape").unwrap_err().status, 404);
    }

    #[tokio::test]
    async fn a_batch_cut_off_by_a_restart_runs_only_the_items_with_no_answer() {
        let hub = crate::testing::hub(&crate::testing::temp_home(), &["alpha"]);
        let id = "bresume01";
        let dir = hub.batches.root().unwrap().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let job = Job {
            id: id.into(),
            name: String::new(),
            model: "alpha".into(),
            system: None,
            max_tokens: 16,
            concurrency: 1,
            prompts: vec!["a".into(), "b".into(), "c".into()],
            state: JobState::Running,
            created: 1,
            finished: None,
        };
        save_job(&dir, &job).unwrap();
        append(
            &dir.join(RESULTS),
            &ItemResult {
                index: 0,
                ok: true,
                answer: Some("answered before the crash".into()),
                error: None,
                completion_tokens: 4,
            },
        )
        .unwrap();
        // the line for item 1 was being written when the hub stopped
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(RESULTS))
            .unwrap()
            .write_all(b"{\"index\":1,\"ok\":tr")
            .unwrap();

        resume(&hub);
        let finished = settled(&hub, id).await;
        assert_eq!(finished["state"], "done", "{finished}");
        let page = results(&hub, id, 0, 10).unwrap();
        let rows = page["results"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "the cut-off line is gone, items 1 and 2 ran: {page}");
        assert_eq!(rows[0]["answer"], "answered before the crash", "item 0 kept its answer");
        let mut indexes: Vec<u64> = rows.iter().map(|r| r["index"].as_u64().unwrap()).collect();
        indexes.sort();
        assert_eq!(indexes, vec![0, 1, 2]);
    }
}

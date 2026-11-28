//! Data-broker opt-outs: f*ck_data_brokers, driven from ModelMistress.
//!
//! [f*ck_data_brokers](https://github.com/gary23w/f-ck_data_brokers) (MIT) is one Python file with no dependencies. It
//! takes the public data-broker registries, writes each broker a legal deletion and opt-out letter in the owner's name,
//! reads the replies and chases the ones that go quiet. ModelMistress does not copy it, rewrite it or bundle it. She
//! fetches that one file, at one pinned commit, only when the owner asks (`privacy.install`), checks its SHA-256 against
//! the value pinned here, and runs it through its own MCP mode.
//!
//! What keeps this safe to have in a hub that also runs models and agents:
//!
//! - **Pinned.** The file is the one at [`COMMIT`]. A different file, whatever its name, is refused; the hash is checked
//!   when it is installed and again before every call.
//! - **Owner only.** The `privacy.*` operations are the owner's. A peer key reaches the model routes only, and Misty has
//!   no privacy tool.
//! - **Nothing is sent without the owner's count.** Writing letters makes drafts on disk. `privacy.send` first says how
//!   many emails would go, from which address and to whom; it sends only when called again with `confirm_count` equal to
//!   that number. The mailbox password (an app password) is never an argument: it is `FDB_SMTP_PASS` in the hub's own
//!   environment, handed to the tool for the one call that sends and never stored or logged. Without it the owner has the
//!   tool's send page, which needs no password: they click one button per letter in their own mail program.
//! - **A clean environment.** The tool runs in isolated Python mode with an environment holding only what Python and the
//!   network need, so none of the hub's tokens or keys reach it.
//! - **Its own folder.** Everything it keeps (the owner's name and email, the broker list, the letters) is under
//!   `<home>/privacy/`.

pub mod cli;
pub mod ops;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::engine::{Fail, Result};

pub const REPO: &str = "gary23w/f-ck_data_brokers";
/// The commit the pinned file is from.
pub const COMMIT: &str = "528fb023821c1e0f9f9241921e723af25f2235c8";
pub const FILE: &str = "fuck_data_brokers.py";
/// SHA-256 and size of that file at that commit (its git blob is 4d5c04b1).
pub const SHA256: &str = "dce00f90cfa888db0116afe68432fdf65bbec28a4239427b4bf20efded2aa093";
pub const SIZE: u64 = 85_839;
/// The most a download may be before it is cut off: the file is under 90 KB.
pub const MAX_SIZE: u64 = 512 * 1024;

pub fn source_url() -> String {
    format!("https://raw.githubusercontent.com/{REPO}/{COMMIT}/{FILE}")
}

/// The hash the installed file must have: the pinned one. (A test pins its own stand-in file on its own thread.)
pub fn pinned() -> String {
    #[cfg(test)]
    if let Some(s) = tests::PIN.with(|p| p.borrow().clone()) {
        return s;
    }
    SHA256.to_string()
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where everything lives.
#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub script: PathBuf,
    pub meta: PathBuf,
    /// The tool's own folder (`FDB_HOME`): the owner's details, the brokers, the requests, the outbox.
    pub state_dir: PathBuf,
    pub state_file: PathBuf,
    pub send_page: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Self {
        let root = home.join("privacy");
        let state_dir = root.join("state");
        Self {
            script: root.join("tool").join(FILE),
            meta: root.join("tool").join("SOURCE.json"),
            state_file: state_dir.join("state.json"),
            send_page: state_dir.join("outbox").join("send.html"),
            state_dir,
            root,
        }
    }
}

// ---- installing ----------------------------------------------------------------------------------------------------

pub enum Source {
    Url(String),
    File(PathBuf),
}

/// Whether the installed file is the pinned one.
pub fn verify(p: &Paths, want_sha: &str) -> std::result::Result<(), String> {
    let bytes = std::fs::read(&p.script).map_err(|_| "it is not installed (`privacy install`)".to_string())?;
    if crate::km::sha256_hex(&bytes).eq_ignore_ascii_case(want_sha) {
        Ok(())
    } else {
        Err("the installed file does not match the pinned hash (it was changed on disk); `privacy install --force` puts the pinned one back".into())
    }
}

/// Fetches or reads the file, checks it against `want_sha`, and only then writes it. A file that does not match is never
/// written, so a bad download leaves nothing behind.
pub async fn install(p: &Paths, http: &reqwest::Client, src: &Source, want_sha: &str, force: bool) -> Result<Value> {
    if !force && verify(p, want_sha).is_ok() {
        return Ok(json!({"installed": true, "already": true, "path": p.script, "sha256": want_sha}));
    }
    let bytes: Vec<u8> = match src {
        Source::File(f) => {
            let len = std::fs::metadata(f)
                .map_err(|e| Fail::invalid(format!("{}: {e}", f.display())))?
                .len();
            if len > MAX_SIZE {
                return Err(Fail::invalid(format!(
                    "{} is {len} bytes; the file is under {MAX_SIZE}",
                    f.display()
                )));
            }
            std::fs::read(f).map_err(|e| Fail::invalid(format!("{}: {e}", f.display())))?
        }
        Source::Url(u) => {
            let mut r = http
                .get(u)
                .timeout(Duration::from_secs(60))
                .send()
                .await
                .map_err(|e| Fail::new(502, "download_failed", format!("{u}: {e}")))?;
            if !r.status().is_success() {
                return Err(Fail::new(502, "download_failed", format!("{u}: {}", r.status())));
            }
            let mut buf: Vec<u8> = vec![];
            while let Some(c) = r
                .chunk()
                .await
                .map_err(|e| Fail::new(502, "download_failed", format!("{u}: {e}")))?
            {
                buf.extend_from_slice(&c);
                if buf.len() as u64 > MAX_SIZE {
                    return Err(Fail::new(
                        502,
                        "download_failed",
                        format!("{u} is larger than {MAX_SIZE} bytes; stopped"),
                    ));
                }
            }
            buf
        }
    };
    let got = crate::km::sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(want_sha) {
        return Err(Fail::new(
            422,
            "hash_mismatch",
            format!("the file hashes to {got}, not the pinned {want_sha}; nothing was installed"),
        ));
    }
    let dir = p.script.parent().unwrap_or(&p.root);
    std::fs::create_dir_all(dir).map_err(|e| Fail::new(500, "io", format!("{}: {e}", dir.display())))?;
    let part = p.script.with_extension("py.part");
    std::fs::write(&part, &bytes).map_err(|e| Fail::new(500, "io", format!("{}: {e}", part.display())))?;
    std::fs::rename(&part, &p.script).map_err(|e| Fail::new(500, "io", format!("{}: {e}", p.script.display())))?;
    let meta = json!({
        "repo": REPO, "commit": COMMIT, "file": FILE, "sha256": got, "size": bytes.len(), "licence": "MIT",
        "installed_at": now_s(),
        "from": match src { Source::Url(u) => u.clone(), Source::File(f) => f.display().to_string() },
    });
    let _ = std::fs::write(&p.meta, serde_json::to_vec_pretty(&meta).unwrap_or_default());
    Ok(
        json!({"installed": true, "already": false, "path": p.script, "sha256": got, "size": bytes.len(), "commit": COMMIT}),
    )
}

// ---- Python --------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Python {
    pub exe: PathBuf,
    /// Arguments before the script's (`-3` for the Windows launcher).
    pub pre: Vec<String>,
    pub version: String,
}

async fn probe(exe: &Path, pre: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(pre)
        .args(["-I", "-c", "import sys;print('%d.%d.%d'%sys.version_info[:3])"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::engine::no_window(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None; // the Microsoft Store's stand-in `python` exits 9009 with a message
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut parts = v.split('.').map(|x| x.parse::<u32>().ok());
    let (major, minor) = (parts.next()??, parts.next()??);
    ((major, minor) >= (3, 8)).then_some(v)
}

/// A Python 3.8 or later: `MM_PRIVACY_PYTHON`, the Forge's interpreter, then `python`, `python3`, `py -3`.
pub async fn find_python(forge_python: Option<PathBuf>) -> std::result::Result<Python, String> {
    let mut tries: Vec<(PathBuf, Vec<&str>)> = vec![];
    if let Some(p) = std::env::var_os("MM_PRIVACY_PYTHON").filter(|p| !p.is_empty()) {
        tries.push((PathBuf::from(p), vec![]));
    }
    if let Some(p) = forge_python {
        tries.push((p, vec![]));
    }
    tries.push((PathBuf::from("python"), vec![]));
    tries.push((PathBuf::from("python3"), vec![]));
    tries.push((PathBuf::from("py"), vec!["-3"]));
    for (exe, pre) in tries {
        if let Some(version) = probe(&exe, &pre).await {
            return Ok(Python {
                exe,
                pre: pre.iter().map(|s| s.to_string()).collect(),
                version,
            });
        }
    }
    Err("no Python 3.8 or later was found; install one, or set MM_PRIVACY_PYTHON to its path".into())
}

// ---- running the tool ----------------------------------------------------------------------------------------------

/// The environment the tool gets: what Python and the network need, and nothing else.
const KEEP_ENV: &[&str] = &[
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "TEMP",
    "TMP",
    "TMPDIR",
    "PATH",
    "HOME",
    "USERPROFILE",
    "LANG",
    "LC_ALL",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    // where and as whom the owner's own mailbox is reached (not the password)
    "FDB_SMTP_HOST",
    "FDB_SMTP_PORT",
    "FDB_SMTP_USER",
];

/// One call at a time: the tool reads and writes its state file without locking.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Runner {
    pub python: Python,
    pub script: PathBuf,
    pub state_dir: PathBuf,
}

impl Runner {
    /// Calls one of the tool's MCP tools and returns what it answered. A tool that refuses (its `isError`) is a 422 with its
    /// own words; they are written to be read by the owner.
    pub async fn call(&self, tool: &str, args: Value, extra_env: &[(&str, &str)], limit: Duration) -> Result<Value> {
        let _one = ONE_AT_A_TIME.lock().await;
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|e| Fail::new(500, "io", format!("{}: {e}", self.state_dir.display())))?;
        let mut cmd = tokio::process::Command::new(&self.python.exe);
        cmd.args(&self.python.pre)
            .args(["-I", "-X", "utf8"])
            .arg(&self.script)
            .arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for k in KEEP_ENV {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        cmd.env("FDB_HOME", &self.state_dir);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        crate::engine::no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| Fail::new(500, "spawn_failed", format!("{}: {e}", self.python.exe.display())))?;
        let input = format!(
            "{}\n{}\n{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "modelmistress", "version": crate::hub::VERSION}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": args}}),
        );
        if let Some(mut stdin) = child.stdin.take() {
            // a tool that has already exited cannot be written to; what it said (or did not) is reported below
            let _ = stdin.write_all(input.as_bytes()).await;
            // dropped here: the end of input ends the tool's loop
        }
        let out = tokio::time::timeout(limit, child.wait_with_output())
            .await
            .map_err(|_| {
                Fail::new(
                    504,
                    "timeout",
                    format!("{tool} took longer than {}s and was stopped", limit.as_secs()),
                )
            })?
            .map_err(|e| Fail::new(500, "io", e.to_string()))?;
        parse_reply(
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            tool,
        )
    }
}

/// The answer to request 2 in the tool's output.
pub fn parse_reply(stdout: &str, stderr: &str, tool: &str) -> Result<Value> {
    let reply = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == 2)
        .ok_or_else(|| {
            let e: String = stderr
                .trim()
                .chars()
                .rev()
                .take(400)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Fail::new(
                502,
                "tool_failed",
                format!(
                    "{tool} gave no answer{}",
                    if e.is_empty() { String::new() } else { format!(": {e}") }
                ),
            )
        })?;
    if let Some(e) = reply.get("error") {
        return Err(Fail::new(
            502,
            "tool_failed",
            format!("{tool}: {}", e["message"].as_str().unwrap_or("error")),
        ));
    }
    let text = reply["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
    if reply["result"]["isError"] == true {
        return Err(Fail::new(422, "refused", text));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

// ---- what is waiting to be sent ------------------------------------------------------------------------------------

/// One email waiting to go.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    pub reference: String,
    /// `request` for a first letter; a follow-up is `reminder`, `answer` and so on.
    pub label: String,
    pub broker: String,
    pub to: String,
}

#[derive(Clone, Debug, Default)]
pub struct Queue {
    /// The owner's address, from the tool's profile.
    pub from: Option<String>,
    pub followups: Vec<Item>,
    pub firsts: Vec<Item>,
}

impl Queue {
    /// How many emails `fdb_send` sends with this `limit`: every follow-up, and the first letters up to the limit.
    pub fn count(&self, limit: usize) -> usize {
        self.followups.len() + self.firsts.len().min(limit)
    }
    /// Everyone who could be written to, follow-ups first.
    pub fn everyone(&self) -> Vec<Item> {
        self.followups.iter().chain(&self.firsts).cloned().collect()
    }
}

/// Reads the tool's state file the way the tool's own `unsent` does. A missing file is an empty queue; one that cannot be
/// read is an error, so a send is refused instead of guessed at.
pub fn read_queue(state_file: &Path) -> std::result::Result<Queue, String> {
    let text = match std::fs::read_to_string(state_file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Queue::default()),
        Err(e) => return Err(format!("{}: {e}", state_file.display())),
    };
    let st: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", state_file.display()))?;
    let broker = |key: &str| -> (String, String) {
        let b = &st["brokers"][key];
        (
            b["name"].as_str().unwrap_or("?").to_string(),
            b["email"].as_str().unwrap_or("").to_string(),
        )
    };
    let mut q = Queue {
        from: st["profile"]["email"].as_str().map(str::to_string),
        ..Default::default()
    };
    if let Some(reqs) = st["requests"].as_object() {
        for (key, r) in reqs {
            if r["state"] == "draft" && r["via"] != "form" {
                let (name, to) = broker(r["broker"].as_str().unwrap_or(""));
                q.firsts.push(Item {
                    reference: r["ref"].as_str().unwrap_or(key).to_string(),
                    label: "request".into(),
                    broker: name,
                    to,
                });
            }
        }
    }
    if let Some(mail) = st["mail"].as_array() {
        for m in mail {
            let Some(r) = m["ref"].as_str().filter(|r| st["requests"].get(*r).is_some()) else {
                continue;
            };
            let (name, _) = broker(st["requests"][r]["broker"].as_str().unwrap_or(""));
            q.followups.push(Item {
                reference: r.to_string(),
                label: m["label"].as_str().unwrap_or("follow-up").to_string(),
                broker: name,
                to: m["to"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(q)
}

#[cfg(test)]
mod tests;

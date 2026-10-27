//! Talking to a running model-mistress hub: find its control file, call its operations, and bridge them to MCP over stdio.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

pub struct Client {
    pub url: String,
    token: String,
    http: reqwest::Client,
}

/// The home folder: `MM_HOME` when it is set to something other than blanks (trimmed, taken as given), else the
/// platform's standard folder.
pub fn default_home() -> PathBuf {
    home_for(std::env::var_os("MM_HOME"))
}

fn home_for(mm_home: Option<OsString>) -> PathBuf {
    let set = mm_home.and_then(|v| match v.to_str() {
        Some(s) => Some(s.trim()).filter(|t| !t.is_empty()).map(PathBuf::from),
        None => Some(PathBuf::from(v)),
    });
    set.unwrap_or_else(platform_home)
}

/// The standard folder for this platform, without `MM_HOME`.
fn platform_home() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("ModelMistress")
}

impl Client {
    /// The hub a client should talk to: the one `MODELMISTRESS_URL` names (with `MODELMISTRESS_TOKEN`), else the one
    /// in <home>/control.json. For a process that cannot see the home folder, such as a packaged app whose
    /// %LOCALAPPDATA% is redirected. Every error names the URL and the file that were checked.
    pub async fn find(home: Option<PathBuf>) -> Result<Self> {
        let home = home.unwrap_or_else(default_home);
        let path = home.join("control.json");
        let env_url = std::env::var("MODELMISTRESS_URL").ok();
        let env_token = std::env::var("MODELMISTRESS_TOKEN").ok();
        let control = match env_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(_) => None,
            None => read_control_file(&path)?,
        };
        let (url, token, source) = hub_address(env_url.as_deref(), env_token.as_deref(), control.as_ref(), &path)?;
        let hub = Self::new(&url, &token);
        hub.health()
            .await
            .with_context(|| format!("model-mistress at {url} is not answering (its address came from {source})"))?;
        Ok(hub)
    }

    /// A client for the hub at `url` with its token (what `find` reads from the control file).
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// The hub's open health answer: {ok, pid, version, uptime_s}.
    pub async fn health(&self) -> Result<Value> {
        Ok(self
            .http
            .get(format!("{}/v1/health", self.url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    // ---- what a program with a window needs: one call each, all through the hub's operations -------------------

    pub async fn status(&self) -> Result<Value> {
        self.call("service.status", json!({})).await
    }

    pub async fn list_models(&self, refresh: bool) -> Result<Vec<Value>> {
        let v = self.call("model.list", json!({"refresh": refresh})).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn loaded_models(&self) -> Result<Vec<Value>> {
        let v = self.call("model.loaded", json!({})).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn load_model(&self, model: &str, opts: Value) -> Result<Value> {
        let mut a = if opts.is_object() { opts } else { json!({}) };
        a["model"] = json!(model);
        self.call("model.load", a).await
    }

    pub async fn unload_model(&self, model: &str) -> Result<Value> {
        self.call("model.unload", json!({"model": model})).await
    }

    /// Start a download; returns the job id (`job` shows progress).
    pub async fn pull_model(&self, model: &str, source: Option<&str>) -> Result<String> {
        let mut a = json!({"model": model});
        if let Some(s) = source {
            a["source"] = json!(s);
        }
        let v = self.call("model.pull", a).await?;
        v["job"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("the hub started no download job"))
    }

    pub async fn job(&self, id: &str) -> Result<Value> {
        self.call("job.get", json!({"job": id})).await
    }

    pub async fn delete_model(&self, model: &str) -> Result<Value> {
        self.call("model.delete", json!({"model": model})).await
    }

    pub async fn search_models(&self, query: &str) -> Result<Vec<Value>> {
        let v = self.call("model.search", json!({"query": query})).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// One chat answer. `messages` is the OpenAI list ([{"role": "user", "content": "..."}]).
    pub async fn chat(&self, model: &str, messages: Vec<Value>) -> Result<Value> {
        self.call("chat.complete", json!({"model": model, "messages": messages}))
            .await
    }

    /// A streamed chat answer through `POST /v1/chat/completions`: `on_text` gets each piece as it arrives; the whole
    /// text is returned.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<Value>,
        mut on_text: impl FnMut(&str),
    ) -> Result<String> {
        use futures::StreamExt;
        let r = self
            .http
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(&self.token)
            .json(&json!({"model": model, "messages": messages, "stream": true}))
            .send()
            .await?;
        if !r.status().is_success() {
            let status = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            bail!(
                "{}",
                v["error"]["message"].as_str().unwrap_or(&format!("HTTP {status}"))
            );
        }
        let (mut all, mut buf) = (String::new(), String::new());
        let mut body = r.bytes_stream();
        while let Some(chunk) = body.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(end) = buf.find("\n\n") {
                let event: String = buf.drain(..end + 2).collect();
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    if let Some(t) = serde_json::from_str::<Value>(data.trim())
                        .ok()
                        .and_then(|v| v["choices"][0]["delta"]["content"].as_str().map(str::to_string))
                    {
                        on_text(&t);
                        all.push_str(&t);
                    }
                }
            }
        }
        Ok(all)
    }

    /// Speech to text of a file on the hub's machine (`speech.transcribe`); `model` and `language` may be empty.
    pub async fn transcribe(&self, file: &str, model: Option<&str>, language: Option<&str>) -> Result<Value> {
        let mut a = json!({"file": file});
        for (k, v) in [("model", model), ("language", language)] {
            if let Some(v) = v {
                a[k] = json!(v);
            }
        }
        self.call("speech.transcribe", a).await
    }

    /// Text to speech into a file on the hub's machine (`speech.synthesize`).
    pub async fn speak(&self, text: &str, output: &str, model: Option<&str>, voice: Option<&str>) -> Result<Value> {
        let mut a = json!({"input": text, "output": output});
        for (k, v) in [("model", model), ("voice", voice)] {
            if let Some(v) = v {
                a[k] = json!(v);
            }
        }
        self.call("speech.synthesize", a).await
    }

    pub async fn create_key(&self, peer: &str, limits: Value) -> Result<Value> {
        let mut a = if limits.is_object() { limits } else { json!({}) };
        a["peer"] = json!(peer);
        self.call("key.create", a).await
    }

    pub async fn keys(&self) -> Result<Vec<Value>> {
        let v = self.call("key.list", json!({})).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn revoke_key(&self, key: &str) -> Result<Value> {
        self.call("key.revoke", json!({"key": key})).await
    }

    pub async fn usage(&self, key: Option<&str>) -> Result<Vec<Value>> {
        let a = key.map(|k| json!({"key": k})).unwrap_or(json!({}));
        let v = self.call("usage.get", a).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn operations(&self) -> Result<Vec<Value>> {
        let v: Value = self
            .http
            .get(format!("{}/v1/operations", self.url))
            .bearer_auth(&self.token)
            .send()
            .await?
            .json()
            .await?;
        Ok(v["operations"].as_array().cloned().unwrap_or_default())
    }

    pub async fn call(&self, op: &str, args: Value) -> Result<Value> {
        let r = self
            .http
            .post(format!("{}/v1/call/{op}", self.url))
            .bearer_auth(&self.token)
            .json(&args)
            .send()
            .await?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(hub_failure(status, &v));
        }
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// The models a peer key may use (`GET /v1/models`, the OpenAI list).
    pub async fn peer_models(&self) -> Result<Value> {
        let r = self
            .http
            .get(format!("{}/v1/models", self.url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        answer(r).await
    }

    /// One answer from a model a peer key may use (`POST /v1/chat/completions`, not streamed).
    pub async fn peer_chat(&self, model: &str, prompt: &str, max_tokens: Option<u64>) -> Result<Value> {
        let mut body = json!({"model": model, "stream": false, "messages": [{"role": "user", "content": prompt}]});
        if let Some(n) = max_tokens {
            body["max_tokens"] = json!(n);
        }
        let r = self
            .http
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        answer(r).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.http
            .post(format!("{}/v1/service/stop", self.url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Ok(())
    }
}

/// MCP tool names allow letters, digits, '_' and '-': "model.list" is served as "model_list".
fn tool_name(op_id: &str) -> String {
    op_id.replace('.', "_")
}

/// The hub's answer: its body on success, else the error a caller can act on.
async fn answer(r: reqwest::Response) -> Result<Value> {
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        Ok(v)
    } else {
        Err(hub_failure(status, &v))
    }
}

/// The error a caller sees: the hub's stable code and its message (`code: message`), else the HTTP status.
fn hub_failure(status: reqwest::StatusCode, body: &Value) -> anyhow::Error {
    let message = body["error"]["message"].as_str().unwrap_or("");
    match body["error"]["code"].as_str() {
        Some(code) if !message.is_empty() => anyhow!("{code}: {message}"),
        _ if !message.is_empty() => anyhow!("{message}"),
        _ => anyhow!("HTTP {status}"),
    }
}

/// The control file as JSON; None when there is no hub's file at `path`.
fn read_control_file(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("{} is not a control file", path.display())),
        Err(_) => Ok(None),
    }
}

/// Where to find the hub: the URL, the token and the source that gave them. `MODELMISTRESS_URL` needs its token from
/// `MODELMISTRESS_TOKEN`; otherwise the control file decides. Pure, so the rules are tested without the environment.
fn hub_address(
    env_url: Option<&str>,
    env_token: Option<&str>,
    control: Option<&Value>,
    control_path: &Path,
) -> Result<(String, String, String)> {
    if let Some(url) = env_url.map(str::trim).filter(|u| !u.is_empty()) {
        let token = env_token.map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| {
            anyhow!("MODELMISTRESS_URL is {url}, but MODELMISTRESS_TOKEN is not set (the hub's token)")
        })?;
        return Ok((url.to_string(), token.to_string(), "MODELMISTRESS_URL".to_string()));
    }
    let Some(doc) = control else {
        bail!(
            "no hub found: {} does not exist and MODELMISTRESS_URL is not set. Start one with `model-mistress serve`, \
             or set MODELMISTRESS_URL and MODELMISTRESS_TOKEN to a running hub.",
            control_path.display()
        );
    };
    let url = doc["url"]
        .as_str()
        .ok_or_else(|| anyhow!("{} has no url", control_path.display()))?;
    let token = doc["token"]
        .as_str()
        .ok_or_else(|| anyhow!("{} has no token", control_path.display()))?;
    Ok((url.to_string(), token.to_string(), control_path.display().to_string()))
}

/// What an MCP client (Claude, for one) is told about this server: what its tools are for and when to use them.
const MCP_INSTRUCTIONS: &str = "ModelMistress serves local models behind one API. `model_list` shows the models on this \
machine and which are loaded. `model_load` loads one into memory and `model_unload` frees it: a load can take minutes and \
a lot of RAM or GPU memory, so unload what you no longer need. `chat_complete` answers one prompt with a model. \
`service_status` shows the hub's version, memory and loaded models. Tools marked destructive delete or change files.";

/// What an MCP session on this server may do: every operation with the owner's token, or only the model routes with a
/// peer key (for an agent that should list models and chat, and nothing that changes the hub).
#[derive(Clone, Copy)]
pub enum McpAccess<'a> {
    Owner(&'a Client),
    Peer(&'a Client),
}

/// What a peer key's session is told: the two tools it has, and that the owner does the rest.
const PEER_MCP_INSTRUCTIONS: &str =
    "This ModelMistress server uses a scoped key. `models_list` shows the models the key \
may use, and `chat_complete` answers one prompt with one of them. Loading, unloading, downloads and settings are not \
available with this key; the owner does those.";

/// The two tools of a peer key: the models it may use, and one answer from one of them.
fn peer_tools() -> Vec<Value> {
    vec![
        json!({"name": "models_list", "description": "List the models this key may use (the OpenAI model list).",
               "inputSchema": {"type": "object", "properties": {}},
               "annotations": {"readOnlyHint": true, "destructiveHint": false}}),
        json!({"name": "chat_complete",
               "description": "Answer one prompt with a model this key may use. The answer is not streamed.",
               "inputSchema": {"type": "object",
                               "properties": {"model": {"type": "string"}, "prompt": {"type": "string"},
                                              "max_tokens": {"type": "integer", "minimum": 1}},
                               "required": ["model", "prompt"]},
               "annotations": {"readOnlyHint": true, "destructiveHint": false}}),
    ]
}

/// Serve the hub's operations as MCP tools over stdio (JSON-RPC 2.0, one message per line), with the owner's token.
pub async fn serve_mcp(hub: Client) -> Result<()> {
    serve_mcp_io(McpAccess::Owner(&hub), std::io::stdin().lock(), std::io::stdout()).await
}

/// The MCP loop over any line reader and writer: stdio for the CLI, buffers in the tests.
pub async fn serve_mcp_io(access: McpAccess<'_>, input: impl BufRead, mut out: impl Write) -> Result<()> {
    // The owner's tools are the hub's operations; a peer key's tools are the two model routes.
    let (instructions, tools, names) = match access {
        McpAccess::Owner(hub) => {
            let ops = hub.operations().await?;
            let tools: Vec<Value> = ops
                .iter()
                .map(|o| {
                    json!({
                        "name": tool_name(o["id"].as_str().unwrap_or("")),
                        "description": o["summary"],
                        "inputSchema": o["input_schema"],
                        "annotations": {"readOnlyHint": !o["mutating"].as_bool().unwrap_or(true),
                                        "destructiveHint": o["destructive"].as_bool().unwrap_or(false)}
                    })
                })
                .collect();
            let names: Vec<(String, String)> = ops
                .iter()
                .filter_map(|o| o["id"].as_str().map(|id| (tool_name(id), id.to_string())))
                .collect();
            (MCP_INSTRUCTIONS, tools, names)
        }
        McpAccess::Peer(_) => (PEER_MCP_INSTRUCTIONS, peer_tools(), Vec::new()),
    };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    out,
                    "{}",
                    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})
                )?;
                out.flush()?;
                continue;
            }
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        }; // a notification: no reply
        let method = msg["method"].as_str().unwrap_or("");
        let reply = match method {
            "initialize" => Ok(
                json!({"protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                                      "capabilities": {"tools": {}},
                                      "serverInfo": {"name": "model-mistress", "version": env!("CARGO_PKG_VERSION")},
                                      "instructions": instructions}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
                let args = if msg["params"]["arguments"].is_null() {
                    json!({})
                } else {
                    msg["params"]["arguments"].clone()
                };
                let outcome = match access {
                    McpAccess::Owner(hub) => match names.iter().find(|(n, _)| *n == name) {
                        Some((_, op)) => Some(hub.call(op, args).await),
                        None => None,
                    },
                    McpAccess::Peer(peer) => match name.as_str() {
                        "models_list" => Some(peer.peer_models().await),
                        "chat_complete" => Some(
                            peer.peer_chat(
                                args["model"].as_str().unwrap_or(""),
                                args["prompt"].as_str().unwrap_or(""),
                                args["max_tokens"].as_u64(),
                            )
                            .await,
                        ),
                        _ => None,
                    },
                };
                match outcome {
                    None => Err((-32602, format!("no tool {name}"))),
                    Some(Ok(v)) => Ok(
                        json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v)?}],
                                             "isError": false}),
                    ),
                    Some(Err(e)) => Ok(json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true})),
                }
            }
            _ => Err((-32601, format!("no method {method}"))),
        };
        let msg = match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        writeln!(out, "{msg}")?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_url_needs_a_token_and_wins_over_the_control_file() {
        let control = json!({"url": "http://127.0.0.1:1", "token": "file"});
        let path = Path::new("/home/mm/control.json");
        let (url, token, source) =
            hub_address(Some("http://10.0.0.5:7474/"), Some(" env-token "), Some(&control), path).unwrap();
        assert_eq!(
            (url.as_str(), token.as_str(), source.as_str()),
            ("http://10.0.0.5:7474/", "env-token", "MODELMISTRESS_URL")
        );
        let err = hub_address(Some("http://10.0.0.5:7474"), None, Some(&control), path).unwrap_err();
        assert!(err.to_string().contains("MODELMISTRESS_TOKEN"), "{err}");
    }

    #[test]
    fn without_the_environment_the_control_file_is_used_and_a_missing_one_is_named() {
        let control = json!({"url": "http://127.0.0.1:9", "token": "t"});
        let path = Path::new("/home/mm/control.json");
        let (url, token, source) = hub_address(None, None, Some(&control), path).unwrap();
        assert_eq!(
            (url.as_str(), token.as_str(), source.as_str()),
            ("http://127.0.0.1:9", "t", "/home/mm/control.json")
        );
        let err = hub_address(None, None, None, path).unwrap_err().to_string();
        assert!(
            err.contains("/home/mm/control.json") && err.contains("MODELMISTRESS_URL"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn mcp_answers_initialize_list_and_call_over_a_stream() {
        let home = crate::testing::temp_home();
        let hub = crate::testing::hub(&home, &["alpha"]);
        let (addr, _task) = crate::testing::serve(hub).await;
        let client = Client::new(&format!("http://{addr}"), crate::testing::TOKEN);
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"service_status\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"no_such_tool\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        );
        let mut out = Vec::new();
        serve_mcp_io(McpAccess::Owner(&client), input.as_bytes(), &mut out)
            .await
            .unwrap();
        let replies: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(replies.len(), 4, "a notification gets no reply");
        assert!(replies[0]["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("model_load"));
        let names: Vec<&str> = replies[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(
            names.contains(&"model_list") && names.contains(&"chat_complete"),
            "{names:?}"
        );
        assert!(replies[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"version\""));
        assert_eq!(replies[3]["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn a_peer_key_lists_its_models_and_is_refused_the_owner_operations() {
        let home = crate::testing::temp_home();
        let hub = crate::testing::hub(&home, &["alpha", "beta"]);
        let (_info, secret) = hub
            .keys
            .create("agent", Default::default(), vec!["alpha".into()])
            .unwrap();
        let (addr, _task) = crate::testing::serve(hub).await;
        let peer = Client::new(&format!("http://{addr}"), &secret);

        let models = peer.peer_models().await.unwrap();
        let ids: Vec<&str> = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(ids.contains(&"alpha") && !ids.contains(&"beta"), "{ids:?}");
        let refused = peer
            .call("model.load", json!({"model": "alpha"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.starts_with("forbidden:") && refused.contains("model routes"),
            "{refused}"
        );

        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"models_list\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"model_load\",",
            "\"arguments\":{\"model\":\"alpha\"}}}\n",
        );
        let mut out = Vec::new();
        serve_mcp_io(McpAccess::Peer(&peer), input.as_bytes(), &mut out)
            .await
            .unwrap();
        let replies: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let names: Vec<&str> = replies[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, ["models_list", "chat_complete"]);
        assert!(replies[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("alpha"));
        assert_eq!(
            replies[2]["error"]["code"], -32602,
            "owner tools are not on a peer key's server"
        );
    }

    #[test]
    fn mm_home_is_the_home_when_set() {
        assert_eq!(home_for(Some("/srv/mm".into())), PathBuf::from("/srv/mm"));
    }

    #[test]
    fn mm_home_is_trimmed_and_not_expanded() {
        assert_eq!(home_for(Some("  /srv/mm  ".into())), PathBuf::from("/srv/mm"));
        assert_eq!(home_for(Some("~/mm".into())), PathBuf::from("~/mm"));
    }

    #[test]
    fn empty_or_blank_mm_home_is_ignored() {
        assert_eq!(home_for(Some("".into())), platform_home());
        assert_eq!(home_for(Some("   ".into())), platform_home());
    }

    #[test]
    fn unset_mm_home_is_the_platform_folder() {
        assert_eq!(home_for(None), platform_home());
        assert!(platform_home().ends_with("ModelMistress"));
    }
}

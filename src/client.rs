//! Talking to a running model-mistress hub: find its control file, call its operations, and bridge them to MCP over stdio.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

pub struct Client {
    pub url: String,
    token: String,
    http: reqwest::Client,
}

pub fn default_home() -> PathBuf {
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
    /// The hub described by <home>/control.json (it must be running).
    pub async fn find(home: Option<PathBuf>) -> Result<Self> {
        let home = home.unwrap_or_else(default_home);
        let path = home.join("control.json");
        let doc: Value = serde_json::from_slice(
            &std::fs::read(&path)
                .with_context(|| format!("model-mistress is not running ({} is missing)", path.display()))?,
        )?;
        let url = doc["url"]
            .as_str()
            .ok_or_else(|| anyhow!("{} has no url", path.display()))?
            .to_string();
        let token = doc["token"]
            .as_str()
            .ok_or_else(|| anyhow!("{} has no token", path.display()))?
            .to_string();
        let hub = Self {
            url,
            token,
            http: reqwest::Client::new(),
        };
        hub.http
            .get(format!("{}/v1/health", hub.url))
            .send()
            .await
            .with_context(|| format!("model-mistress at {} is not answering (a stale control file?)", hub.url))?;
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
            bail!(
                "{}",
                v["error"]["message"].as_str().unwrap_or(&format!("HTTP {status}"))
            );
        }
        Ok(v.get("result").cloned().unwrap_or(v))
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

/// Serve the hub's operations as MCP tools over stdio (JSON-RPC 2.0, one message per line).
pub async fn serve_mcp(hub: Client) -> Result<()> {
    let ops = hub.operations().await?;
    let names: Vec<(String, String)> = ops
        .iter()
        .filter_map(|o| o["id"].as_str().map(|id| (tool_name(id), id.to_string())))
        .collect();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
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
                                      "serverInfo": {"name": "model-mistress", "version": env!("CARGO_PKG_VERSION")}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": ops.iter().map(|o| json!({
                "name": tool_name(o["id"].as_str().unwrap_or("")),
                "description": o["summary"],
                "inputSchema": o["input_schema"],
                "annotations": {"readOnlyHint": !o["mutating"].as_bool().unwrap_or(true),
                                "destructiveHint": o["destructive"].as_bool().unwrap_or(false)}})).collect::<Vec<_>>()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                match names.iter().find(|(n, _)| n == name) {
                    None => Err((-32602, format!("no tool {name}"))),
                    Some((_, op)) => {
                        let args = msg["params"]["arguments"].clone();
                        Ok(
                            match hub.call(op, if args.is_null() { json!({}) } else { args }).await {
                                Ok(v) => {
                                    json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v)?}], "isError": false})
                                }
                                Err(e) => {
                                    json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true})
                                }
                            },
                        )
                    }
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

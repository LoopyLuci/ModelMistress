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

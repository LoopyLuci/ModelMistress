//! `model-mistress`: the hub (`serve`, the default) and a client for a running hub.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use model_mistress::client::{default_home, serve_mcp, Client};
use model_mistress::engine::{Engine, Settings};
use model_mistress::hub::{self, new_token, Hub, ServeOpts};

#[derive(Parser)]
#[command(
    name = "model-mistress",
    version,
    about = "Load and serve local models (GGUF through llama.cpp) \
behind one OpenAI-compatible API and a control API for programs and agents."
)]
struct Args {
    /// Where the control file, settings and logs live.
    #[arg(long, global = true, env = "MM_HOME")]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the hub in the foreground (the default).
    Serve {
        /// Address to listen on. Anything but 127.0.0.1 exposes the API (token-protected) to the network.
        #[arg(long, default_value = "127.0.0.1")]
        host: std::net::IpAddr,
        /// Port (0 = a free one).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Is the hub running? What is loaded?
    Status,
    /// Stop the hub (it unloads every model).
    Stop,
    /// List the hub's operations.
    Ops,
    /// Run one operation: `call model.load '{"model": "llama3.2:1b"}'`.
    Call { op: String, args: Option<String> },
    /// The models on this machine.
    Models {
        #[arg(long)]
        refresh: bool,
    },
    /// Load a model.
    Load { model: String },
    /// Unload a model ("*" for all).
    Unload { model: String },
    /// One question, one answer.
    Chat { model: String, prompt: String },
    /// Serve the operations as MCP tools over stdio (the hub must be running).
    Mcp,
}

fn main() {
    let args = Args::parse();
    let home = args.home.clone().unwrap_or_else(default_home);
    let rt = tokio::runtime::Runtime::new().expect("a tokio runtime");
    let code = rt.block_on(async move {
        match run(
            args.cmd.unwrap_or(Cmd::Serve {
                host: [127, 0, 0, 1].into(),
                port: 0,
            }),
            home,
        )
        .await
        {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("model-mistress: {e:#}");
                1
            }
        }
    });
    std::process::exit(code);
}

fn show(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

async fn run(cmd: Cmd, home: PathBuf) -> anyhow::Result<()> {
    let client = || Client::find(Some(home.clone()));
    match cmd {
        Cmd::Serve { host, port } => {
            std::fs::create_dir_all(&home)?;
            let settings = Settings::load(&home).map_err(anyhow::Error::msg)?;
            let engine = {
                let home = home.clone();
                tokio::task::spawn_blocking(move || Engine::new(home, settings)).await?
            };
            eprintln!(
                "model-mistress: {} model(s) found on this machine",
                engine.catalog().len()
            );
            let token = std::env::var("MM_TOKEN")
                .ok()
                .filter(|t| t.len() >= 16)
                .unwrap_or_else(new_token);
            let hub = Hub {
                engine: engine.clone(),
                token: Arc::new(token),
                started: Instant::now(),
                stop: Arc::new(tokio::sync::Notify::new()),
            };
            hub::serve(hub, ServeOpts { home, host, port }).await?;
            drop(engine);
        }
        Cmd::Status => {
            let c = client().await?;
            show(&c.call("service.status", json!({})).await?);
        }
        Cmd::Stop => {
            client().await?.stop().await?;
            println!("stopping");
        }
        Cmd::Ops => {
            for o in client().await?.operations().await? {
                println!(
                    "{:<16} {}",
                    o["id"].as_str().unwrap_or(""),
                    o["summary"].as_str().unwrap_or("")
                );
            }
        }
        Cmd::Call { op, args } => {
            let a: Value = match args {
                Some(s) => serde_json::from_str(&s)?,
                None => json!({}),
            };
            show(&client().await?.call(&op, a).await?);
        }
        Cmd::Models { refresh } => {
            let v = client().await?.call("model.list", json!({"refresh": refresh})).await?;
            for m in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<52} {:>7} GB  {:<12} {:<8} {}",
                    m["id"].as_str().unwrap_or(""),
                    m["size_gb"],
                    m["architecture"].as_str().unwrap_or("-"),
                    m["file_type"].as_str().unwrap_or("-"),
                    if m["loaded"] == true { "loaded" } else { "" }
                );
            }
        }
        Cmd::Load { model } => show(&client().await?.call("model.load", json!({"model": model})).await?),
        Cmd::Unload { model } => show(&client().await?.call("model.unload", json!({"model": model})).await?),
        Cmd::Chat { model, prompt } => {
            let v = client()
                .await?
                .call("chat.complete", json!({"model": model, "prompt": prompt}))
                .await?;
            println!("{}", v["text"].as_str().unwrap_or(""));
            eprintln!(
                "[{} | {} ms | {:.1} tokens/s]",
                v["model"].as_str().unwrap_or(""),
                v["ms"],
                v["tokens_per_s"].as_f64().unwrap_or(0.0)
            );
        }
        Cmd::Mcp => serve_mcp(client().await?).await?,
    }
    Ok(())
}

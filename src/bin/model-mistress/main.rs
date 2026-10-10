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
    /// Download a model: `pull hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M` (Hugging Face) or `pull qwen3:8b` (the Ollama
    /// library).
    Pull {
        model: String,
        /// huggingface or ollama, when the name is ambiguous.
        #[arg(long)]
        source: Option<String>,
    },
    /// Search Hugging Face for GGUF models.
    Search { query: String },
    /// Delete a model ModelMistress downloaded.
    Rm { model: String },
    /// One question, one answer.
    Chat { model: String, prompt: String },
    /// Speech to text with the built-in whisper.cpp (WAV or MP3): `transcribe talk.wav --model base.en`.
    Transcribe {
        file: PathBuf,
        /// A whisper model (`base.en` for ggml-base.en.bin); default: the whisper_model setting or what is found.
        #[arg(long)]
        model: Option<String>,
        /// ISO-639-1 code such as `de`; default: detected.
        #[arg(long)]
        language: Option<String>,
        /// Text that guides the style or spelling.
        #[arg(long)]
        prompt: Option<String>,
        /// Translate the speech into English.
        #[arg(long)]
        translate: bool,
        /// text, json, verbose_json, srt or vtt.
        #[arg(long, default_value = "text")]
        format: String,
    },
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
                jobs: Arc::new(Default::default()),
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
        Cmd::Pull { model, source } => {
            let c = client().await?;
            let mut a = json!({"model": model});
            if let Some(s) = source {
                a["source"] = json!(s);
            }
            let job = c.call("model.pull", a).await?;
            let id = job["job"].as_str().unwrap_or("").to_string();
            loop {
                let j = c.call("job.get", json!({"job": id})).await?;
                let status = j["status"].as_str().unwrap_or("");
                eprint!(
                    "\r{} {:>5.1}%  {:.2}/{:.2} GB  {:.1} MB/s  {}          ",
                    j["model"].as_str().unwrap_or(""),
                    j["percent"].as_f64().unwrap_or(0.0),
                    j["done_bytes"].as_f64().unwrap_or(0.0) / 1e9,
                    j["total_bytes"].as_f64().unwrap_or(0.0) / 1e9,
                    j["mb_per_s"].as_f64().unwrap_or(0.0),
                    j["message"].as_str().unwrap_or("")
                );
                if status != "running" {
                    eprintln!();
                    if status != "done" {
                        anyhow::bail!("{status}: {}", j["message"].as_str().unwrap_or(""));
                    }
                    println!("{}", j["result"]["model"].as_str().unwrap_or(""));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Cmd::Search { query } => {
            let v = client().await?.call("model.search", json!({"query": query})).await?;
            for m in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<60} {:>10} downloads",
                    m["pull"].as_str().unwrap_or(""),
                    m["downloads"]
                );
            }
        }
        Cmd::Rm { model } => show(&client().await?.call("model.delete", json!({"model": model})).await?),
        Cmd::Transcribe {
            file,
            model,
            language,
            prompt,
            translate,
            format,
        } => {
            // The hub reads the file, and its working folder is not ours.
            let file = std::path::absolute(&file)?;
            let mut a = json!({"file": file, "translate": translate, "format": format});
            for (k, v) in [("model", model), ("language", language), ("prompt", prompt)] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            let v = client().await?.call("speech.transcribe", a).await?;
            print!("{}", v["rendered"].as_str().unwrap_or(""));
        }
        Cmd::Mcp => serve_mcp(client().await?).await?,
    }
    Ok(())
}

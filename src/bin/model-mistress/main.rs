//! `model-mistress`: the hub (`serve`, the default) and a client for a running hub.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use model_mistress::client::{default_home, serve_mcp, serve_mcp_io, Client, McpAccess};
use model_mistress::engine::{Engine, Settings};
use model_mistress::hosting::Keys;
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
    #[arg(long, global = true)]
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
    /// One model's details (Ollama's `show`): modelfile, parameters, template, model info.
    Show { model: String },
    /// The loaded models (Ollama's `ps`).
    Ps,
    /// Copy a model preset, or name a plain model as a preset (Ollama's `cp`).
    Cp { source: String, destination: String },
    /// Register a named model preset from a Modelfile: `create mymodel -f Modelfile`.
    Create {
        name: String,
        /// The Modelfile (FROM, SYSTEM, PARAMETER, TEMPLATE, ADAPTER, MESSAGE).
        #[arg(short, long, default_value = "Modelfile")]
        file: PathBuf,
    },
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
    /// Recognise a picture with an ONNX model in <home>/vision: `vision resnet50 cat.jpg [--top-k 5]`. A classifier
    /// lists its best guesses, a detector the boxes it found, an embedding model prints its vector as JSON.
    Vision {
        /// A vision model (see `models`), or `-` for the only one.
        model: String,
        /// A PNG or JPEG file.
        image: PathBuf,
        /// How many predictions (default 5) or detections (default 100).
        #[arg(long)]
        top_k: Option<u32>,
        /// The least score of a box (default 0.25) or prediction.
        #[arg(long)]
        threshold: Option<f64>,
        /// Print the hub's whole answer as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Text to speech with llama.cpp's llama-tts: `speak "Hello there" --out hello.wav [--model M] [--voice V]`.
    Speak {
        text: String,
        /// Where to write the audio (WAV, or raw PCM with --format pcm).
        #[arg(long, short, default_value = "speech.wav")]
        out: PathBuf,
        /// A speech model (default: the tts_model setting, else the only one found).
        #[arg(long)]
        model: Option<String>,
        /// A recording in <home>/voices (name without extension) or the path of one.
        #[arg(long)]
        voice: Option<String>,
        /// 0.25 to 4; 1 is normal speed.
        #[arg(long)]
        speed: Option<f64>,
        /// ISO-639-1 code, for models that take one.
        #[arg(long)]
        language: Option<String>,
        /// wav or pcm.
        #[arg(long)]
        format: Option<String>,
    },
    /// Generate an image with the built-in stable-diffusion.cpp: `image "a red fox in snow" --out fox.png [--model M]
    /// [--size 512x512] [--steps 20]`. See docs/images.md for the models.
    Image {
        prompt: String,
        /// Where to write the PNG (with --count above 1: fox_0.png, fox_1.png, ...).
        #[arg(long, short, default_value = "image.png")]
        out: PathBuf,
        /// An image model (default: the image_model setting, else the only one found).
        #[arg(long)]
        model: Option<String>,
        /// What to leave out.
        #[arg(long)]
        negative: Option<String>,
        /// How many images.
        #[arg(long = "count", short = 'n')]
        count: Option<u32>,
        /// WIDTHxHEIGHT, each a multiple of 8 (default 512x512).
        #[arg(long)]
        size: Option<String>,
        /// Sampling steps (default 20).
        #[arg(long)]
        steps: Option<u32>,
        /// Guidance scale (1 for FLUX models).
        #[arg(long)]
        cfg_scale: Option<f64>,
        /// Sampling method, for example euler, euler_a or dpm++2m.
        #[arg(long)]
        sampler: Option<String>,
        /// A number that makes the picture repeatable (default: random).
        #[arg(long)]
        seed: Option<i64>,
    },
    /// Make a smaller copy of a model with llama-quantize: `quantize llama3.2:1b Q4_K_M`, or a .gguf file's path.
    Quantize {
        /// A model id from `models`, or the path of a .gguf file.
        model: String,
        /// Q4_K_M, Q5_K_M, Q6_K, Q8_0, IQ4_XS, F16, ...
        r#type: String,
        /// Where to write it (default: <home>/models/<name>-<TYPE>.gguf).
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// The model is already quantized.
        #[arg(long)]
        allow_requantize: bool,
        /// Replace the output file if it exists.
        #[arg(long)]
        overwrite: bool,
        /// An importance matrix file.
        #[arg(long)]
        imatrix: Option<PathBuf>,
    },
    /// LoRA adapters of a model: `lora <model>` lists, `lora <model> attach a.gguf [--scale 0.8]`, `detach [a]`,
    /// `scale a 0.5`.
    Lora {
        model: String,
        /// list, attach, detach or scale.
        action: Option<String>,
        adapter: Option<String>,
        /// The adapter's strength (attach: default 1; scale: the new value).
        #[arg(long)]
        scale: Option<f64>,
    },
    /// A draft model for speculative decoding: `draft <model> <draft model>`, `draft <model> --off`, `draft <model>`
    /// shows it.
    Draft {
        model: String,
        draft_model: Option<String>,
        #[arg(long)]
        off: bool,
        /// The most tokens drafted at a time.
        #[arg(long)]
        n_max: Option<u32>,
    },
    /// Keys for peers (other machines) that use this hub's models: `keys create alice --requests-per-min 60`.
    Keys {
        #[command(subcommand)]
        cmd: KeysCmd,
    },
    /// What each peer key has used (requests, tokens, refused requests): `usage` or `usage alice`.
    Usage {
        /// A key id or a peer name (default: all).
        key: Option<String>,
    },
    /// Knowledge Modules: `km list`, `km search <text>`, `km import <path>`, `km load <id>`... (`km help` lists them).
    Km {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The Knowledge Module factory: `factory create <goal> --author M --source dir:docs`, `factory list`,
    /// `factory get <task>`, `factory resume <task>`, `factory review` (`factory help` lists them).
    Factory {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The Forge, ModelMistress's training system: `forge status`, `forge install`, `forge preflight`,
    /// `forge run <step> <json>`, `forge runs`, `forge show <run>`, `forge cancel <run>`.
    Forge {
        #[command(subcommand)]
        cmd: ForgeCmd,
    },
    /// Serve the operations as MCP tools over stdio. The hub must be running, or pass `--start` to start one in the
    /// background; it keeps running after this exits (`stop` ends it).
    Mcp {
        /// Start a hub for this home when none answers.
        #[arg(long)]
        start: bool,
        /// Serve only the model routes, with a peer key from MODELMISTRESS_KEY: `models_list` and `chat_complete`.
        #[arg(long)]
        peer: bool,
    },
}

#[derive(Subcommand)]
enum ForgeCmd {
    /// Where the Forge lives, whether it is installed, the last preflight.
    Status,
    /// Create the Forge's Python environment, install the pinned requirements (PyTorch for ROCm), run the preflight.
    Install {
        /// The Python that creates the environment (default: forge_python, MM_FORGE_PYTHON, else `python`).
        #[arg(long)]
        python: Option<String>,
        /// A requirements file (default: forge/requirements-rocm-windows.txt).
        #[arg(long)]
        requirements: Option<PathBuf>,
        #[arg(long)]
        skip_preflight: bool,
    },
    /// Prove that an optimizer step really runs on the GPU.
    Preflight,
    /// Run a pipeline step with JSON arguments and follow it.
    Run {
        step: String,
        args: Option<String>,
        /// Start it and return at once.
        #[arg(long)]
        detach: bool,
        /// Stop after this many seconds.
        #[arg(long)]
        timeout_s: Option<u64>,
    },
    /// The runs, newest first.
    Runs {
        #[arg(long)]
        step: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// One run: result, metrics, artifacts, the end of its log.
    Show {
        run: String,
        /// Only this metric.
        #[arg(long)]
        metric: Option<String>,
    },
    /// Stop a run and its process tree.
    Cancel { run: String },
    /// Load a model on the engine its format needs: `load <id or path> [--device auto|gpu|cpu] [--plan-only]`.
    Load {
        model: String,
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        plan_only: bool,
    },
    /// Generate with a model the Forge worker serves (tokens with logprobs as JSON).
    Generate {
        model: String,
        prompt: String,
        #[arg(long, default_value_t = 64)]
        max_tokens: u32,
        #[arg(long, default_value_t = 0.0)]
        temperature: f64,
        #[arg(long, default_value_t = 0)]
        top_logprobs: u32,
    },
    /// Stop the Forge worker serving a model.
    Unload { model: String },
}

/// Follows a job until it ends, showing its progress on stderr; returns its final description.
async fn follow(c: &Client, job: &Value) -> anyhow::Result<Value> {
    let id = job["job"].as_str().unwrap_or("").to_string();
    loop {
        let j = c.call("job.get", json!({"job": id})).await?;
        let status = j["status"].as_str().unwrap_or("").to_string();
        eprint!(
            "\r{:>5.1}%  {:<70}",
            j["percent"].as_f64().unwrap_or(0.0),
            j["message"].as_str().unwrap_or("").chars().take(70).collect::<String>()
        );
        if status != "running" {
            eprintln!();
            return Ok(j);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Make a key for a peer. It is shown once; only its hash is kept.
    Create {
        peer: String,
        /// Requests per minute.
        #[arg(long)]
        requests_per_min: Option<u64>,
        /// Prompt plus completion tokens per UTC day.
        #[arg(long)]
        tokens_per_day: Option<u64>,
        /// Requests at a time.
        #[arg(long)]
        concurrent: Option<u64>,
        /// A model the key may use (repeatable; a trailing * is a prefix). Default: every model.
        #[arg(long = "model")]
        models: Vec<String>,
    },
    /// The keys (never the secrets).
    List,
    /// Revoke a key by id, or all of a peer's keys by name. It stops working at once.
    Revoke { key: String },
}

/// The key commands go through a running hub; with none running they work on the files in the home folder (the next
/// `serve` reads them).
async fn key_op(home: &std::path::Path, op: &str, args: Value) -> anyhow::Result<Value> {
    match Client::find(Some(home.to_path_buf())).await {
        Ok(c) => c.call(op, args).await,
        Err(_) => {
            let keys = Keys::open(home);
            let out = hub::key_call(&keys, op, &args).map_err(|f| anyhow::anyhow!("{}", f.message));
            keys.flush();
            out
        }
    }
}

/// The hub's home: `--home` when given, else `MM_HOME`, else the standard folder.
fn home_from_args(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(default_home)
}

fn main() {
    let args = Args::parse();
    let home = home_from_args(args.home.clone());
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
            let hub = Hub::new(engine.clone(), token);
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
        Cmd::Show { model } => {
            let v = client().await?.call("model.show", json!({"model": model})).await?;
            println!("{}", v["modelfile"].as_str().unwrap_or(""));
            show(&json!({"details": v["details"], "parameters": v["parameters"], "capabilities": v["capabilities"]}));
        }
        Cmd::Ps => {
            let v = client().await?.call("model.loaded", json!({})).await?;
            println!("{:<48} {:>9}  {:>8}", "NAME", "VRAM MiB", "IDLE s");
            for m in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<48} {:>9}  {:>8}",
                    m["model"].as_str().unwrap_or(""),
                    m["serving"]["vram_mib"]
                        .as_f64()
                        .map_or("-".into(), |x| format!("{x:.0}")),
                    m["idle_s"]
                );
            }
        }
        Cmd::Cp { source, destination } => show(
            &client()
                .await?
                .call("model.copy", json!({"source": source, "destination": destination}))
                .await?,
        ),
        Cmd::Create { name, file } => {
            let text = anyhow::Context::with_context(std::fs::read_to_string(&file), || file.display().to_string())?;
            // the hub resolves relative ADAPTER paths against the Modelfile's folder
            let dir = std::path::absolute(&file)?.parent().map(|d| d.to_path_buf());
            show(
                &client()
                    .await?
                    .call(
                        "model.create",
                        json!({"model": name, "modelfile": text, "base_dir": dir}),
                    )
                    .await?,
            );
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
        Cmd::Vision {
            model,
            image,
            top_k,
            threshold,
            json,
        } => {
            // The hub reads the file, and its working folder is not ours.
            let image = std::path::absolute(&image)?;
            let mut a = json!({"file": image});
            if model != "-" {
                a["model"] = json!(model);
            }
            if let Some(v) = top_k {
                a["top_k"] = json!(v);
            }
            if let Some(v) = threshold {
                a["threshold"] = json!(v);
            }
            let v = client().await?.call("vision.analyze", a).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print!("{}", model_mistress::vision::render_text(&v));
            }
        }
        Cmd::Speak {
            text,
            out,
            model,
            voice,
            speed,
            language,
            format,
        } => {
            // The hub writes the file, and its working folder is not ours.
            let out = std::path::absolute(&out)?;
            let mut a = json!({"input": text, "output": out});
            for (k, v) in [
                ("model", model),
                ("voice", voice),
                ("language", language),
                ("response_format", format),
            ] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            if let Some(v) = speed {
                a["speed"] = json!(v);
            }
            let v = client().await?.call("speech.synthesize", a).await?;
            println!("{}", v["output"].as_str().unwrap_or(""));
            eprintln!(
                "[{} | {} s of audio | {} bytes]",
                v["model"].as_str().unwrap_or(""),
                v["seconds"],
                v["bytes"]
            );
        }
        Cmd::Keys { cmd } => match cmd {
            KeysCmd::Create {
                peer,
                requests_per_min,
                tokens_per_day,
                concurrent,
                models,
            } => {
                let mut a = json!({"peer": peer, "models": models});
                for (k, v) in [
                    ("requests_per_min", requests_per_min),
                    ("tokens_per_day", tokens_per_day),
                    ("concurrent", concurrent),
                ] {
                    if let Some(v) = v {
                        a[k] = json!(v);
                    }
                }
                let v = key_op(&home, "key.create", a).await?;
                println!("{}", v["key"].as_str().unwrap_or(""));
                eprintln!(
                    "[key {} for {}: shown once, store it now; it is the peer's API key (Authorization: Bearer ...)]",
                    v["id"].as_str().unwrap_or(""),
                    v["peer"].as_str().unwrap_or("")
                );
            }
            KeysCmd::List => {
                let v = key_op(&home, "key.list", json!({})).await?;
                for k in v.as_array().cloned().unwrap_or_default() {
                    println!(
                        "{:<10} {:<24} {:<9} {}",
                        k["id"].as_str().unwrap_or(""),
                        k["peer"].as_str().unwrap_or(""),
                        if k["revoked"].is_null() { "active" } else { "revoked" },
                        k["quotas"]
                    );
                }
            }
            KeysCmd::Revoke { key } => show(&key_op(&home, "key.revoke", json!({"key": key})).await?),
        },
        Cmd::Usage { key } => {
            let a = key.map(|k| json!({"key": k})).unwrap_or(json!({}));
            let v = key_op(&home, "usage.get", a).await?;
            for u in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<10} {:<24} {:>7} requests {:>5} refused {:>9} tokens today {:>9} tokens in all{}",
                    u["id"].as_str().unwrap_or(""),
                    u["peer"].as_str().unwrap_or(""),
                    u["requests"],
                    u["rejected"],
                    u["tokens_today"],
                    u["prompt_tokens"].as_u64().unwrap_or(0) + u["completion_tokens"].as_u64().unwrap_or(0),
                    if u["revoked"].is_null() { "" } else { "  (revoked)" }
                );
            }
        }
        Cmd::Image {
            prompt,
            out,
            model,
            negative,
            count,
            size,
            steps,
            cfg_scale,
            sampler,
            seed,
        } => {
            // The hub writes the file, and its working folder is not ours.
            let out = std::path::absolute(&out)?;
            let mut a = json!({"prompt": prompt, "output": out});
            for (k, v) in [
                ("model", model),
                ("negative_prompt", negative),
                ("size", size),
                ("sampler", sampler),
            ] {
                if let Some(v) = v {
                    a[k] = json!(v);
                }
            }
            if let Some(v) = count {
                a["n"] = json!(v);
            }
            if let Some(v) = steps {
                a["steps"] = json!(v);
            }
            if let Some(v) = cfg_scale {
                a["cfg_scale"] = json!(v);
            }
            if let Some(v) = seed {
                a["seed"] = json!(v);
            }
            let c = client().await?;
            let job = c.call("image.generate", a).await?;
            let id = job["job"].as_str().unwrap_or("").to_string();
            loop {
                let j = c.call("job.get", json!({"job": id})).await?;
                let status = j["status"].as_str().unwrap_or("");
                eprint!(
                    "\r{:>5.1}%  {}          ",
                    j["percent"].as_f64().unwrap_or(0.0),
                    j["message"].as_str().unwrap_or("")
                );
                if status != "running" {
                    eprintln!();
                    if status != "done" {
                        anyhow::bail!("{status}: {}", j["message"].as_str().unwrap_or(""));
                    }
                    for f in j["result"]["files"].as_array().into_iter().flatten() {
                        println!("{}", f.as_str().unwrap_or(""));
                    }
                    eprintln!(
                        "[{} | seed {} | {} s]",
                        j["result"]["model"].as_str().unwrap_or(""),
                        j["result"]["seed"],
                        j["result"]["seconds"]
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        Cmd::Quantize {
            model,
            r#type,
            out,
            allow_requantize,
            overwrite,
            imatrix,
        } => {
            let c = client().await?;
            let mut a = json!({"type": r#type, "allow_requantize": allow_requantize, "overwrite": overwrite});
            // a file path is the input; anything else is a model id
            if std::path::Path::new(&model).is_file() {
                a["input"] = json!(std::path::absolute(&model)?);
            } else {
                a["model"] = json!(model);
            }
            if let Some(o) = out {
                a["output"] = json!(std::path::absolute(&o)?);
            }
            if let Some(i) = imatrix {
                a["imatrix"] = json!(std::path::absolute(&i)?);
            }
            let job = c.call("quantize", a).await?;
            let id = job["job"].as_str().unwrap_or("").to_string();
            loop {
                let j = c.call("job.get", json!({"job": id})).await?;
                let status = j["status"].as_str().unwrap_or("");
                eprint!(
                    "\r{:>5.1}%  {}          ",
                    j["percent"].as_f64().unwrap_or(0.0),
                    j["message"].as_str().unwrap_or("")
                );
                if status != "running" {
                    eprintln!();
                    if status != "done" {
                        anyhow::bail!("{status}: {}", j["message"].as_str().unwrap_or(""));
                    }
                    println!("{}", j["result"]["output"].as_str().unwrap_or(""));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Cmd::Lora {
            model,
            action,
            adapter,
            scale,
        } => {
            let mut a = json!({"model": model, "action": action.unwrap_or_else(|| "list".into())});
            if let Some(ad) = adapter {
                // a file on this machine is given to the hub by its full path
                let p = std::path::Path::new(&ad);
                a["adapter"] = if p.is_file() {
                    json!(std::path::absolute(p)?)
                } else {
                    json!(ad)
                };
            }
            if let Some(s) = scale {
                a["scale"] = json!(s);
            }
            show(&client().await?.call("lora", a).await?);
        }
        Cmd::Draft {
            model,
            draft_model,
            off,
            n_max,
        } => {
            let mut a = json!({"model": model, "off": off});
            if let Some(d) = draft_model {
                let p = std::path::Path::new(&d);
                a["draft_model"] = if p.is_file() {
                    json!(std::path::absolute(p)?)
                } else {
                    json!(d)
                };
            }
            if let Some(n) = n_max {
                a["n_max"] = json!(n);
            }
            show(&client().await?.call("speculative", a).await?);
        }
        Cmd::Km { args } => model_mistress::km::cli::run(&home, args).await?,
        Cmd::Factory { args } => model_mistress::factory::cli::run(&home, args).await?,
        Cmd::Forge { cmd } => {
            let c = client().await?;
            match cmd {
                ForgeCmd::Status => show(&c.call("forge.status", json!({})).await?),
                ForgeCmd::Install {
                    python,
                    requirements,
                    skip_preflight,
                } => {
                    let mut a = json!({"skip_preflight": skip_preflight});
                    if let Some(p) = python {
                        a["python"] = json!(p);
                    }
                    if let Some(r) = requirements {
                        a["requirements"] = json!(std::path::absolute(&r)?);
                    }
                    let job = c.call("forge.install", a).await?;
                    eprintln!("run {}", job["run"].as_str().unwrap_or(""));
                    let j = follow(&c, &job).await?;
                    if j["status"] != "done" {
                        anyhow::bail!(
                            "{}: {}",
                            j["status"].as_str().unwrap_or(""),
                            j["message"].as_str().unwrap_or("")
                        );
                    }
                    show(&j["result"]);
                }
                ForgeCmd::Preflight => show(&c.call("forge.preflight", json!({})).await?),
                ForgeCmd::Run {
                    step,
                    args,
                    detach,
                    timeout_s,
                } => {
                    let a: Value = match args {
                        Some(s) => serde_json::from_str(&s)?,
                        None => json!({}),
                    };
                    let mut req = json!({"step": step, "args": a});
                    if let Some(t) = timeout_s {
                        req["timeout_s"] = json!(t);
                    }
                    let job = c.call("forge.run", req).await?;
                    eprintln!("run {}", job["run"].as_str().unwrap_or(""));
                    if detach {
                        show(&job);
                    } else {
                        let j = follow(&c, &job).await?;
                        if j["status"] != "done" {
                            anyhow::bail!(
                                "{}: {}",
                                j["status"].as_str().unwrap_or(""),
                                j["message"].as_str().unwrap_or("")
                            );
                        }
                        show(&j["result"]["result"]);
                    }
                }
                ForgeCmd::Runs { step, status, limit } => {
                    let mut a = json!({"limit": limit});
                    if let Some(s) = step {
                        a["step"] = json!(s);
                    }
                    if let Some(s) = status {
                        a["status"] = json!(s);
                    }
                    for r in c.call("forge.runs", a).await?.as_array().cloned().unwrap_or_default() {
                        println!(
                            "{:<26} {:<14} {:<11} {:>8}  {}",
                            r["run"].as_str().unwrap_or(""),
                            r["step"].as_str().unwrap_or(""),
                            r["status"].as_str().unwrap_or(""),
                            r["seconds"].as_f64().map_or("-".to_string(), |x| format!("{x:.1}s")),
                            r["error"].as_str().unwrap_or("").lines().next().unwrap_or("")
                        );
                    }
                }
                ForgeCmd::Show { run, metric } => {
                    let mut a = json!({"run": run});
                    if let Some(m) = metric {
                        a["metric"] = json!(m);
                    }
                    show(&c.call("forge.run.get", a).await?);
                }
                ForgeCmd::Cancel { run } => show(&c.call("forge.run.cancel", json!({"run": run})).await?),
                ForgeCmd::Load {
                    model,
                    device,
                    plan_only,
                } => {
                    let mut a = json!({"model": model, "plan_only": plan_only});
                    if let Some(d) = device {
                        a["device"] = json!(d);
                    }
                    show(&c.call("forge.load", a).await?);
                }
                ForgeCmd::Generate {
                    model,
                    prompt,
                    max_tokens,
                    temperature,
                    top_logprobs,
                } => show(
                    &c.call(
                        "forge.generate",
                        json!({"model": model, "prompt": prompt, "max_tokens": max_tokens,
                               "temperature": temperature, "top_logprobs": top_logprobs}),
                    )
                    .await?,
                ),
                ForgeCmd::Unload { model } => show(&c.call("forge.unload", json!({"model": model})).await?),
            }
        }
        Cmd::Mcp { start, peer } => {
            let hub = match client().await {
                Ok(hub) => hub,
                Err(_) if start => {
                    start_background_hub(&home).await?;
                    client().await?
                }
                Err(e) => return Err(e),
            };
            if peer {
                let key = std::env::var("MODELMISTRESS_KEY").map_err(|_| {
                    anyhow::anyhow!(
                        "--peer needs a peer key in MODELMISTRESS_KEY (`model-mistress keys create <name>`)"
                    )
                })?;
                let scoped = Client::new(&hub.url, key.trim());
                serve_mcp_io(McpAccess::Peer(&scoped), std::io::stdin().lock(), std::io::stdout()).await?
            } else {
                serve_mcp(hub).await?
            }
        }
    }
    Ok(())
}

/// Windows hands every inheritable handle to a child process. A hub started from here would keep this process's stdio
/// pipes open, and whoever reads them (an MCP client, for one) would wait for the hub to exit. So the standard handles
/// stop being inheritable first; the parent keeps using them.
#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    extern "system" {
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
        fn SetHandleInformation(object: *mut c_void, mask: u32, flags: u32) -> i32;
    }

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;

    pub fn stop_inheriting_stdio() {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle takes a standard-handle selector and returns this process's handle table entry;
            // SetHandleInformation only clears the inherit flag of that entry.
            unsafe {
                let handle = GetStdHandle(which);
                if !handle.is_null() && handle as isize != -1 {
                    SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
}

/// Start `model-mistress serve` for this home in the background, detached from this process (it keeps running when
/// the caller exits), and wait until it answers. Its output goes to `<home>/hub.log`.
async fn start_background_hub(home: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    std::fs::create_dir_all(home)?;
    let log = std::fs::File::create(home.join("hub.log")).context("creating hub.log")?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("--home")
        .arg(home)
        .arg("serve")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        win::stop_inheriting_stdio();
    }
    cmd.spawn().context("starting the hub in the background")?;
    for _ in 0..600 {
        if Client::find(Some(home.to_path_buf())).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "the hub started in the background did not answer within 60 seconds (see {})",
        home.join("hub.log").display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: it sets MM_HOME, and a second test doing the same in parallel would race on the variable.
    #[test]
    fn home_flag_beats_mm_home_and_mm_home_is_the_default() {
        std::env::set_var("MM_HOME", "/from-env");
        let flagged = Args::try_parse_from(["model-mistress", "--home", "/explicit", "status"]).unwrap();
        let bare = Args::try_parse_from(["model-mistress", "status"]).unwrap();
        let with_flag = home_from_args(flagged.home.clone());
        let without_flag = home_from_args(bare.home.clone());
        std::env::remove_var("MM_HOME");
        assert_eq!(with_flag, PathBuf::from("/explicit"));
        assert_eq!(without_flag, PathBuf::from("/from-env"));
    }
}

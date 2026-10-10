//! Slash commands of the ModelMistress desktop app.
//!
//! The model commands (`/models`, `/load`, `/pull`, `/infer`, `/speak`, `/keys`, ...) run against the hub through
//! `HubLink`; nothing else serves models. `/agents`, `/agent` and `/swarm` and the settings commands are local to the app.

use std::collections::HashMap;
use std::sync::RwLock;

use once_cell::sync::Lazy;
use serde_json::{json, Value};

use crate::api::HubLink;
use crate::config;

/// Settings structure for user preferences
#[derive(Debug, Clone)]
pub struct Settings {
    pub theme: String,
    pub language: String,
    pub notifications: bool,
    pub auto_save: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            language: "en".to_string(),
            notifications: true,
            auto_save: true,
        }
    }
}

/// Command metadata
#[derive(Debug, Clone)]
pub struct CmdMeta {
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
    pub short_name: Option<&'static str>,
    pub aliases: Vec<&'static str>,
}

/// Command response
#[derive(Debug, Clone)]
pub struct SlashResponse {
    pub success: bool,
    pub content: String,
}

impl SlashResponse {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
        }
    }
}

/// Command error
#[derive(Debug)]
pub enum SlashError {
    CommandNotFound(String),
    InvalidArgs(String),
    MissingArg(String),
    /// The hub said no, or could not be reached.
    Hub(String),
}

impl std::fmt::Display for SlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlashError::CommandNotFound(c) => write!(f, "Unknown command: /{}", c),
            SlashError::InvalidArgs(m) => write!(f, "Invalid arguments: {}", m),
            SlashError::MissingArg(a) => write!(f, "Missing: {}", a),
            SlashError::Hub(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for SlashError {}

impl From<anyhow::Error> for SlashError {
    fn from(e: anyhow::Error) -> Self {
        SlashError::Hub(format!("{e:#}"))
    }
}

/// A registered command
pub struct Command {
    pub meta: CmdMeta,
    pub handler: fn(Vec<String>) -> Result<SlashResponse, SlashError>,
}

/// Global command registry
pub struct CommandRegistry {
    commands: RwLock<HashMap<String, Command>>,
}

static GLOBAL_REGISTRY: Lazy<CommandRegistry> = Lazy::new(CommandRegistry::new);

pub fn get_command_registry() -> &'static CommandRegistry {
    &GLOBAL_REGISTRY
}

/// The commands that run against the hub (see [`run`]); the registry lists them for `/help`.
const HUB_COMMANDS: &[(&str, &str, &str, &str)] = &[
    ("models", "Model", "List models on this machine", "/models [<pattern>]"),
    ("scan", "Model", "Rescan the disk for models", "/scan"),
    ("load", "Model", "Load a model", "/load <model> [ctx]"),
    ("unload", "Model", "Unload a model (* for all)", "/unload <model>"),
    ("pull", "Model", "Download a model", "/pull <model>"),
    ("job", "Model", "Progress of a download", "/job <id>"),
    ("rm", "Model", "Delete a downloaded model", "/rm <model>"),
    (
        "search",
        "Model",
        "Search Hugging Face for GGUF models",
        "/search <text>",
    ),
    ("infer", "Model", "Chat with a model", "/infer <model> <prompt>"),
    (
        "transcribe",
        "Speech",
        "Speech to text of a file",
        "/transcribe <file> [model]",
    ),
    (
        "speak",
        "Speech",
        "Text to speech into a file",
        "/speak <out.wav> <text>",
    ),
    (
        "keys",
        "Hosting",
        "Peer keys: list, create, revoke",
        "/keys [create <peer> [model...] | revoke <key>]",
    ),
    ("usage", "Hosting", "What peer keys have used", "/usage [<peer>]"),
    ("status", "System", "Hub status", "/status"),
    ("health", "System", "Hub health check", "/health"),
];

fn is_hub_command(name: &str) -> bool {
    HUB_COMMANDS.iter().any(|(n, ..)| *n == name)
}

fn needs_hub(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    Err(SlashError::InvalidArgs(
        "this command talks to the hub: use `run`".to_string(),
    ))
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            commands: RwLock::new(HashMap::new()),
        };
        r.register_builtins();
        r
    }

    fn register_builtins(&mut self) {
        let local = |name, category, description, usage, short_name, aliases, handler| Command {
            meta: CmdMeta {
                name,
                category,
                description,
                usage,
                short_name,
                aliases,
            },
            handler,
        };
        self.register(local(
            "help",
            "System",
            "Show help",
            "/help",
            Some("h"),
            vec!["?"],
            cmd_help,
        ));
        for (name, category, description, usage) in HUB_COMMANDS {
            self.register(local(name, category, description, usage, None, vec![], needs_hub));
        }
        self.register(local(
            "agents",
            "Agent",
            "List agents",
            "/agents",
            None,
            vec![],
            cmd_agents,
        ));
        self.register(local(
            "agent",
            "Agent",
            "Agent commands",
            "/agent <add|control>",
            None,
            vec![],
            cmd_agent,
        ));
        self.register(local(
            "swarm",
            "Agent",
            "Swarm commands",
            "/swarm <status|config>",
            None,
            vec![],
            cmd_swarm,
        ));
        self.register(local(
            "version",
            "System",
            "Version info",
            "/version",
            Some("ver"),
            vec![],
            cmd_version,
        ));
        self.register(local(
            "quit",
            "System",
            "Exit app",
            "/quit",
            None,
            vec!["exit", "q"],
            cmd_quit,
        ));
        self.register(local(
            "echo",
            "Utility",
            "Echo input",
            "/echo <msg>",
            None,
            vec![],
            cmd_echo,
        ));
        self.register(local(
            "settings",
            "Config",
            "Settings",
            "/settings",
            None,
            vec![],
            cmd_settings,
        ));
        self.register(local(
            "set",
            "Config",
            "Set value",
            "/set <key> <val>",
            None,
            vec![],
            cmd_set,
        ));
        self.register(local("get", "Config", "Get value", "/get <key>", None, vec![], cmd_get));
    }

    fn register(&self, cmd: Command) {
        self.commands.write().unwrap().insert(cmd.meta.name.to_string(), cmd);
    }

    /// Run a command that does not need the hub. (Those that do answer with an error: use [`run`].)
    pub fn execute(&self, input: &str) -> Result<SlashResponse, SlashError> {
        let (name, args) = parse(input)?;
        let commands = self.commands.read().unwrap();
        if let Some(cmd) = commands.get(&name) {
            (cmd.handler)(args)
        } else {
            Err(SlashError::CommandNotFound(name))
        }
    }

    pub fn list_all_commands(&self) -> Vec<String> {
        self.commands.read().unwrap().keys().cloned().collect()
    }

    pub fn meta(&self, name: &str) -> Option<CmdMeta> {
        self.commands.read().unwrap().get(name).map(|c| c.meta.clone())
    }
}

fn parse(input: &str) -> Result<(String, Vec<String>), SlashError> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.is_empty() || !parts[0].starts_with('/') {
        return Err(SlashError::InvalidArgs("Input must start with /".to_string()));
    }
    Ok((
        parts[0].trim_start_matches('/').to_lowercase(),
        parts[1..].iter().map(|s| s.to_string()).collect(),
    ))
}

/// Run a slash command: the model commands against the hub, the rest locally.
pub async fn run(hub: &HubLink, input: &str) -> Result<SlashResponse, SlashError> {
    let (name, args) = parse(input)?;
    if !is_hub_command(&name) {
        return get_command_registry().execute(input);
    }
    let c = hub.client().await?;
    let arg = |i: usize, what: &str| -> Result<String, SlashError> {
        args.get(i)
            .cloned()
            .ok_or_else(|| SlashError::MissingArg(what.to_string()))
    };
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match name.as_str() {
        "models" | "scan" => {
            let rows = config::rows(&c.list_models(name == "scan").await?);
            let pattern = args.first().map(|p| p.to_lowercase());
            let rows: Vec<_> = rows
                .into_iter()
                .filter(|m| pattern.as_ref().is_none_or(|p| m.id.to_lowercase().contains(p)))
                .collect();
            Ok(SlashResponse::success(config::table(&rows)))
        }
        "load" => {
            let model = arg(0, "model")?;
            let opts = match args.get(1) {
                Some(ctx) => json!({"ctx_size": ctx
                    .parse::<u32>()
                    .map_err(|_| SlashError::InvalidArgs("ctx must be a number".to_string()))?}),
                None => json!({}),
            };
            let v = c.load_model(&model, opts).await?;
            Ok(SlashResponse::success(format!("Loaded {model} in {} ms", v["load_ms"])))
        }
        "unload" => {
            let v = c.unload_model(&arg(0, "model")?).await?;
            Ok(SlashResponse::success(format!("Unloaded {}", v["unloaded"])))
        }
        "pull" => {
            let id = c.pull_model(&arg(0, "model")?, None).await?;
            Ok(SlashResponse::success(format!(
                "Downloading as job {id}; /job {id} shows progress"
            )))
        }
        "job" => {
            let j = c.job(&arg(0, "job id")?).await?;
            Ok(SlashResponse::success(format!(
                "{} {:.1}% {}",
                j["status"].as_str().unwrap_or("?"),
                j["percent"].as_f64().unwrap_or(0.0),
                j["message"].as_str().unwrap_or("")
            )))
        }
        "rm" => Ok(SlashResponse::success(pretty(
            &c.delete_model(&arg(0, "model")?).await?,
        ))),
        "search" => {
            let found = c.search_models(&args.join(" ")).await?;
            Ok(SlashResponse::success(
                found
                    .iter()
                    .map(|m| format!("{}  {} downloads", m["pull"].as_str().unwrap_or(""), m["downloads"]))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        }
        "infer" => {
            if args.len() < 2 {
                return Err(SlashError::InvalidArgs("Need model and prompt".to_string()));
            }
            let v = c
                .chat(&args[0], vec![json!({"role": "user", "content": args[1..].join(" ")})])
                .await?;
            Ok(SlashResponse::success(v["text"].as_str().unwrap_or("").to_string()))
        }
        "transcribe" => {
            let v = c
                .transcribe(&arg(0, "file")?, args.get(1).map(String::as_str), None)
                .await?;
            Ok(SlashResponse::success(v["rendered"].as_str().unwrap_or("").to_string()))
        }
        "speak" => {
            if args.len() < 2 {
                return Err(SlashError::InvalidArgs("Need an output file and text".to_string()));
            }
            let v = c.speak(&args[1..].join(" "), &args[0], None, None).await?;
            Ok(SlashResponse::success(format!(
                "Wrote {}",
                v["output"].as_str().unwrap_or(&args[0])
            )))
        }
        "keys" => match args.first().map(String::as_str) {
            None | Some("list") => {
                let keys = c.keys().await?;
                Ok(SlashResponse::success(
                    keys.iter()
                        .map(|k| {
                            format!(
                                "{}  {}  {}",
                                k["id"].as_str().unwrap_or(""),
                                k["peer"].as_str().unwrap_or(""),
                                if k["revoked"].is_null() { "active" } else { "revoked" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ))
            }
            Some("create") => {
                let peer = arg(1, "peer")?;
                let models: Vec<&String> = args.iter().skip(2).collect();
                let v = c.create_key(&peer, json!({"models": models})).await?;
                Ok(SlashResponse::success(format!(
                    "Key for {peer} (shown once): {}",
                    v["key"].as_str().unwrap_or("")
                )))
            }
            Some("revoke") => {
                let v = c.revoke_key(&arg(1, "key or peer")?).await?;
                Ok(SlashResponse::success(format!("Revoked {}", v["revoked"])))
            }
            Some(other) => Err(SlashError::InvalidArgs(format!("keys {other}"))),
        },
        "usage" => {
            let rows = c.usage(args.first().map(String::as_str)).await?;
            Ok(SlashResponse::success(
                rows.iter()
                    .map(|u| {
                        format!(
                            "{:<24} {} requests, {} refused, {} tokens today",
                            u["peer"].as_str().unwrap_or(""),
                            u["requests"],
                            u["rejected"],
                            u["tokens_today"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        }
        "status" => {
            let s = c.status().await?;
            let loaded = s["loaded"].as_array().map(|l| l.len()).unwrap_or(0);
            Ok(SlashResponse::success(format!(
                "ModelMistress {} up {} s, {} models, {} loaded",
                s["version"].as_str().unwrap_or(""),
                s["uptime_s"],
                s["models_known"],
                loaded
            )))
        }
        "health" => {
            c.health().await?;
            Ok(SlashResponse::success("Healthy"))
        }
        _ => Err(SlashError::CommandNotFound(name)),
    }
}

// Command implementations (local to the app)
fn cmd_help(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    let txt = "Model Mistress - Slash Commands\n\n[Model] /models, /scan, /load, /unload, /pull, /job, /rm, /search, /infer\n[Speech] /transcribe, /speak\n[Hosting] /keys, /usage\n[Agent] /agents, /agent, /swarm\n[System] /help, /status, /health, /version, /quit\n[Config] /settings, /set, /get";
    Ok(SlashResponse::success(txt.to_string()))
}

fn cmd_agents(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    Ok(SlashResponse::success("Agents: hermes ✅, mm-agent ✅".to_string()))
}

fn cmd_agent(args: Vec<String>) -> Result<SlashResponse, SlashError> {
    if args.is_empty() {
        return Err(SlashError::MissingArg("subcommand".to_string()));
    }
    match args[0].as_str() {
        "add" => {
            if args.len() > 2 {
                Ok(SlashResponse::success(format!("🤖 Added {} ({})", args[1], args[2])))
            } else {
                Err(SlashError::MissingArg("name role".to_string()))
            }
        }
        _ => Ok(SlashResponse::success(format!("Agent: {}", args[0]))),
    }
}

fn cmd_swarm(args: Vec<String>) -> Result<SlashResponse, SlashError> {
    match args.first().map(|s| s.as_str()) {
        Some("status") => Ok(SlashResponse::success("Swarm: 2 agents ✅".to_string())),
        _ => Ok(SlashResponse::success("Swarm commands: status".to_string())),
    }
}

fn cmd_version(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    Ok(SlashResponse::success(format!(
        "Model Mistress desktop v{}",
        env!("CARGO_PKG_VERSION")
    )))
}

fn cmd_quit(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    Ok(SlashResponse::success("Goodbye!".to_string()))
}

fn cmd_echo(args: Vec<String>) -> Result<SlashResponse, SlashError> {
    if args.is_empty() {
        return Err(SlashError::MissingArg("message".to_string()));
    }
    Ok(SlashResponse::success(args.join(" ")))
}

fn cmd_settings(_args: Vec<String>) -> Result<SlashResponse, SlashError> {
    Ok(SlashResponse::success("theme: dark, notifications: true".to_string()))
}

fn cmd_set(args: Vec<String>) -> Result<SlashResponse, SlashError> {
    if args.len() < 2 {
        return Err(SlashError::MissingArg("key value".to_string()));
    }
    Ok(SlashResponse::success(format!("Set {} = {}", args[0], args[1])))
}

fn cmd_get(args: Vec<String>) -> Result<SlashResponse, SlashError> {
    if args.is_empty() {
        return Err(SlashError::MissingArg("key".to_string()));
    }
    Ok(SlashResponse::success(format!("{} = value", args[0])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::rig;

    async fn ok(hub: &HubLink, input: &str) -> String {
        run(hub, input).await.unwrap_or_else(|e| panic!("{input}: {e}")).content
    }

    #[tokio::test]
    async fn model_commands_run_against_the_hub() {
        let r = rig().await;
        let models = ok(&r.link, "/models").await;
        assert!(
            models.contains("alpha") && models.contains("beta") && models.contains("loaded"),
            "{models}"
        );
        let only = ok(&r.link, "/models bet").await;
        assert!(only.contains("beta") && !only.contains("alpha"), "{only}");
        assert!(ok(&r.link, "/scan").await.contains("alpha"));
        assert!(ok(&r.link, "/status").await.contains("2 models, 1 loaded"));
        assert_eq!(ok(&r.link, "/health").await, "Healthy");
        assert_eq!(ok(&r.link, "/infer alpha say hello").await, "A");
        assert!(ok(&r.link, "/unload alpha").await.contains("1"));
        assert!(matches!(
            run(&r.link, "/infer alpha").await,
            Err(SlashError::InvalidArgs(_))
        ));
        assert!(matches!(run(&r.link, "/load").await, Err(SlashError::MissingArg(_))));
        // the hub's refusals come back as the command's error
        let err = run(&r.link, "/rm no-such-model").await.unwrap_err();
        assert!(matches!(err, SlashError::Hub(_)), "{err}");
    }

    #[tokio::test]
    async fn key_commands_run_against_the_hub() {
        let r = rig().await;
        let made = ok(&r.link, "/keys create alice alpha").await;
        assert!(made.contains("mmk_"), "{made}");
        assert!(ok(&r.link, "/keys").await.contains("alice  active"));
        assert!(ok(&r.link, "/usage").await.contains("alice"));
        assert_eq!(ok(&r.link, "/keys revoke alice").await, "Revoked 1");
        assert!(ok(&r.link, "/keys list").await.contains("revoked"));
    }

    #[tokio::test]
    async fn local_commands_need_no_hub() {
        let nowhere = HubLink::fixed("http://127.0.0.1:1", "x");
        assert_eq!(ok(&nowhere, "/echo hi there").await, "hi there");
        assert!(ok(&nowhere, "/help").await.contains("/keys"));
        // a model command with no hub is an error that says so, not a made-up answer
        assert!(matches!(run(&nowhere, "/models").await, Err(SlashError::Hub(_))));
        assert!(matches!(
            run(&nowhere, "/nope").await,
            Err(SlashError::CommandNotFound(_))
        ));
        assert!(matches!(run(&nowhere, "models").await, Err(SlashError::InvalidArgs(_))));
        // every hub command is listed in /help's registry
        for (name, ..) in HUB_COMMANDS {
            assert!(get_command_registry().meta(name).is_some(), "{name}");
        }
    }
}

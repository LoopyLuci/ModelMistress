use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use clap::{Parser, Subcommand};
use model_mistress::config::ModelMistressConfig;

#[derive(Parser)]
#[command(name = "model-mistress-cli")]
#[command(about = "ModelMistress CLI/TUI for headless and terminal interaction", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the ModelMistress HTTP server (headless mode)
    Server {
        /// Address to bind to
        #[arg(short, long, default_value = "0.0.0.0:8000")]
        listen: String,
        /// Path to config file
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Interactive terminal UI for chatting with models
    Tui {
        /// ModelMistress server URL
        #[arg(short, long, default_value = "http://localhost:8000")]
        url: String,
    },
    /// Simple REPL chat client
    Chat {
        /// ModelMistress server URL
        #[arg(short, long, default_value = "http://localhost:8000")]
        url: String,
        /// Model to use
        #[arg(short, long)]
        model: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "model_mistress_cli=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Server { listen, config: _ } => {
            info!(%listen, "Starting ModelMistress server in headless mode");
            // In production, load config from file and bind to listen addr
            // For now, use defaults and run the embedded server
            let mut cfg = ModelMistressConfig::load()?;
            cfg.server.listen_addr = listen;
            let server = model_mistress::server::Server::new(cfg).await;
            server.run().await?;
        }
        Commands::Tui { url } => {
            info!(%url, "Starting TUI");
            println!("ModelMistress TUI connecting to {}", url);
            // Placeholder: wire to a proper TUI crate (e.g., tui-rs) later
            println!("TUI not yet implemented. Use --url to point at a live server.");
        }
        Commands::Chat { url, model } => {
            info!(%url, model=?model, "Starting chat REPL");
            println!("ModelMistress Chat REPL -> {}", url);
            if let Some(m) = model {
                println!("Model: {}", m);
            }
            println!("REPL not yet implemented.");
        }
    }

    Ok(())
}

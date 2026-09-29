use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use model_mistress::config::ModelMistressConfig;
use model_mistress::server::Server;
use model_mistress::observability::Observability;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "model_mistress=info,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("🚀 Model Mistress v0.1.0 starting up");

    let config = ModelMistressConfig::load()?;
    info!(listen_addr = %config.server.listen_addr, "Configuration loaded");

    let _observability = Observability::new();
    info!("Observability initialized");

    let server = Server::new(config).await;
    info!("Server created, starting...");

    server.run().await?;

    Ok(())
}

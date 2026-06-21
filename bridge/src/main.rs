// Re-use the library crate's modules instead of declaring them again.
use vibe_bridge::{collector, hub, state};

use anyhow::Result;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, serde::Deserialize)]
struct Config {
    token: String,
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_host() -> String { "0.0.0.0".into() }
fn default_port() -> u16 { 5151 }

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let config_path = std::env::args().nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let cfg_text = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {config_path}: {e}"))?;
    let cfg: Config = toml::from_str(&cfg_text)?;

    let store = Arc::new(state::Store::new());

    // background collector loop: scan ~/.claude/projects every 2s
    {
        let store = store.clone();
        tokio::spawn(async move {
            loop {
                collector::scan_claude(&store);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    let app = hub::create_router(store, cfg.token.clone());
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("vibe-bridge listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
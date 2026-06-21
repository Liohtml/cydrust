// Self-contained Rust hub: scans Claude/Codex/OpenCode/Hermes sessions, polls
// usage, computes per-model metrics + titles, and serves /state — the all-Rust
// replacement for the Python vibemonitor hub.
use vibe_bridge::{collector, collector_hermes, collector_opencode, federation, hub, metrics, state, usage};

use anyhow::Result;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, RwLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

const GONE_TTL: f64 = 14400.0; // 4h

#[derive(Debug, serde::Deserialize)]
struct PriceEntry {
    input: f64,
    output: f64,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
struct FederationConfig {
    #[serde(default = "default_role")]
    role: String,                 // "standalone" | "node" | "aggregator"
    #[serde(default)]
    upstream: Option<String>,     // node: aggregator base URL, e.g. http://aggregator:5151
    #[serde(default)]
    node_id: Option<String>,      // defaults to hostname
}
fn default_role() -> String { "standalone".into() }

#[derive(Debug, serde::Deserialize)]
struct Config {
    token: String,
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    pricing: HashMap<String, PriceEntry>,
    #[serde(default)]
    federation: FederationConfig,
}

fn default_host() -> String { "0.0.0.0".into() }
fn default_port() -> u16 { 5151 }

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".to_string());
    let cfg_text = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {config_path}: {e}"))?;
    let cfg: Config = toml::from_str(&cfg_text)?;

    // pricing: model-substring -> (input_$/1M, output_$/1M)
    let pricing: HashMap<String, (f64, f64)> = cfg
        .pricing
        .iter()
        .map(|(k, v)| (k.clone(), (v.input, v.output)))
        .collect();

    let store = Arc::new(state::Store::new());
    let shared = Arc::new(RwLock::new(state::Shared::default()));
    let remote = Arc::new(federation::RemoteStore::new());  // sessions from other machines

    // ── session scan + reaper loop (2s) ─────────────────────────────────────
    {
        let store = store.clone();
        thread::spawn(move || loop {
            collector::scan_claude(&store);
            collector::scan_codex(&store);
            collector_opencode::scan_opencode(&store);
            collector_hermes::scan_hermes(&store);
            store.remove_gone(now_secs(), GONE_TTL);
            thread::sleep(Duration::from_secs(2));
        });
    }

    // ── usage loop (60s): Claude/Codex API gauges ───────────────────────────
    {
        let shared = shared.clone();
        thread::spawn(move || loop {
            let claude = usage::claude_usage();
            let codex = usage::codex_usage();
            {
                let mut s = shared.write().unwrap();
                s.claude_usage = claude;
                s.codex_usage = codex;
            }
            thread::sleep(Duration::from_secs(60));
        });
    }

    // ── metrics loop (120s): per-model tokens/cost ──────────────────────────
    {
        let shared = shared.clone();
        let pricing = pricing.clone();
        thread::spawn(move || loop {
            let m = metrics::summarize_metrics(now_secs(), &pricing);
            shared.write().unwrap().metrics = m;
            thread::sleep(Duration::from_secs(120));
        });
    }

    // ── titles loop (120s): first-prompt session summaries ──────────────────
    {
        let shared = shared.clone();
        thread::spawn(move || loop {
            let t = metrics::build_titles(now_secs());
            shared.write().unwrap().titles = t;
            thread::sleep(Duration::from_secs(120));
        });
    }

    // ── federation node push loop (2s): send local rows to the aggregator ───
    if cfg.federation.role == "node" {
        if let Some(upstream) = cfg.federation.upstream.clone() {
            let node_id = cfg.federation.node_id.clone().unwrap_or_else(federation::hostname);
            let store = store.clone();
            let shared = shared.clone();
            let token = cfg.token.clone();
            thread::spawn(move || loop {
                let rows = {
                    let sh = shared.read().unwrap();
                    hub::derive_rows_pub(&store, &sh, now_secs())
                };
                let payload = federation::from_session_rows(&rows, &node_id);
                if let Err(e) = federation::push(&payload, &upstream, &token) {
                    tracing::warn!("federation push failed: {e}");
                }
                thread::sleep(Duration::from_secs(2));
            });
        } else {
            tracing::warn!("federation role=node but no upstream set; not pushing");
        }
    }

    let app = hub::create_router(store, shared, cfg.token, remote);
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("vibe-bridge (all-Rust hub) listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

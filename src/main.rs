mod config;
mod hue;
mod server;

use std::net::SocketAddr;

use anyhow::Context;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, server::HueServer};

/// Environment variable holding the listen address (`ip:port`).
const BIND_ENV: &str = "HUE_MCP_BIND";
/// Loopback only by default: there is no TLS or authentication yet.
const DEFAULT_BIND: &str = "127.0.0.1:8080";
const MCP_PATH: &str = "/mcp";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Loaded before logging is set up, so that `.env` can also set `RUST_LOG`.
    let dotenv_warning = config::load_dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    if let Some(warning) = dotenv_warning {
        tracing::warn!("{warning}");
    }

    let bind = std::env::var(BIND_ENV).unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid {BIND_ENV} value: {bind:?}"))?;

    let config = Config::from_env();
    tracing::info!(
        bridge_host = config
            .bridge_host
            .as_deref()
            .unwrap_or("<discovered via mDNS>"),
        app_key_configured = config.app_key.is_some(),
        bridge_id_pinned = config.bridge_id.is_some(),
        "Hue bridge configuration loaded"
    );
    let hue_server = HueServer::new(&config).context("cannot set up the Hue bridge client")?;

    let service: StreamableHttpService<HueServer, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(hue_server.clone()),
        Default::default(),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service(MCP_PATH, service);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    tracing::info!("hue-mcp listening on http://{addr}{MCP_PATH}");

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .context("HTTP server error")
}

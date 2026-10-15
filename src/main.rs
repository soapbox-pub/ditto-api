use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use ditto_api::config::Config;
use ditto_api::db::Db;
use ditto_api::{AppState, app};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        // Under systemd stdout is the journal, which shows escapes literally.
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let config = Config::from_env()?;
    let db = Db::open(&config.db_path)
        .with_context(|| format!("opening {}", config.db_path.display()))?;
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on {addr}, API at {}", config.api_url);
    axum::serve(listener, app(AppState::new(config, db)))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let mut term = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

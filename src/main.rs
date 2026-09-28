//! codexpool — точка входа. Один процесс, два HTTP-сервера (proxy и stats), фоновые таски
//! (проактивный refresh, опрос wham/usage, batch-writer статистики, ретенция).

mod auth;
mod cli;
mod config;
mod import;
mod pool;
mod proxy;
mod router;
mod stats;
mod store;
mod upstream;

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "codexpool=info,tower_http=warn".into()))
        .init();

    let cli = cli::Cli::parse();
    let cfg = Arc::new(config::Config::load(cli.config.as_deref())?);

    match cli.command {
        cli::Command::Serve => serve(cfg).await,
        cli::Command::Import(args) => import::run(&cfg, args).await,
        cli::Command::Accounts(args) => cli::accounts(&cfg, args).await,
        cli::Command::Login(args) => auth::login::run(&cfg, args).await,
        cli::Command::Usage(args) => cli::usage(&cfg, args).await,
        cli::Command::Bench(args) => cli::bench(&cfg, args).await,
    }
}

async fn serve(cfg: Arc<config::Config>) -> Result<()> {
    let store = Arc::new(store::Store::open(&cfg.data_dir.join("codexpool.db"))?);
    store.migrate()?;

    let pool = Arc::new(pool::Pool::load(store.clone(), &cfg)?);
    let upstream = Arc::new(upstream::Upstream::new(&cfg.upstream)?);
    let (stats_tx, stats_writer) = stats::Collector::spawn(store.clone(), &cfg.stats);
    let router = Arc::new(router::Router::new(&cfg, pool.clone()));

    // Фоновые таски: refresh за lead_s до exp, опрос usage, ретенция.
    tokio::spawn(auth::refresh::background_loop(pool.clone(), upstream.clone(), cfg.refresh.clone()));
    tokio::spawn(pool::usage_poll_loop(pool.clone(), upstream.clone(), cfg.refresh.clone()));
    tokio::spawn(stats::retention_loop(store.clone(), cfg.stats.clone()));

    let app_state = proxy::AppState { cfg: cfg.clone(), pool: pool.clone(), router, upstream, stats: stats_tx };
    let proxy_app = proxy::app(app_state.clone());
    let stats_app = stats::api::app(stats::api::StatsState { cfg: cfg.clone(), store, pool });

    let proxy_listener = tokio::net::TcpListener::bind(&cfg.listen).await?;
    let stats_listener = tokio::net::TcpListener::bind(&cfg.stats_listen).await?;
    tracing::info!(listen = %cfg.listen, stats = %cfg.stats_listen, accounts = app_state.pool.len(), "codexpool up");

    tokio::try_join!(
        async { axum::serve(proxy_listener, proxy_app).await.map_err(anyhow::Error::from) },
        async { axum::serve(stats_listener, stats_app).await.map_err(anyhow::Error::from) },
        async { stats_writer.await.map_err(anyhow::Error::from) },
    )?;
    Ok(())
}

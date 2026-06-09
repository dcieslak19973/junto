//! junto — the host/app entry point.
//!
//! NOTE: junto is **terminal-less for humans** (CLAUDE.md constraint #2). This
//! binary is the host *process*, not a human-facing terminal UI; `serve`
//! starts the long-running host that agents (and, later, human surfaces)
//! connect to. Binary/`main` code may use `anyhow` and may `?`-propagate —
//! unlike the library crates.

mod mcp;
mod render;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

#[derive(Parser)]
#[command(name = "junto", about = "junto host process", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP write surface over streamable HTTP (docs/adr/0012).
    ///
    /// Agents connect at http://127.0.0.1:<port>/mcp and author ledger
    /// entries in the given repository's refs/junto/* record.
    Serve {
        /// The git repository holding the durable record.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Port to bind on localhost. 1727: the year Franklin founded the Junto.
        #[arg(long, default_value_t = 1727)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Serve { repo, port } => serve(repo, port).await,
    }
}

/// Run the host until Ctrl-C: the MCP write surface at /mcp, and the
/// read-only channel pages at /channels/{name} (+ /brief for hooks/agents).
async fn serve(repo: PathBuf, port: u16) -> Result<()> {
    let repo = repo.canonicalize()?;
    // One ledger, shared by the MCP tools and the web read routes.
    let ledger: web::SharedLedger = Arc::new(tokio::sync::Mutex::new(junto_kernel::Ledger::new(
        junto_substrate_git::GitRefsSubstrate::open(repo.clone()),
    )));
    let handler = mcp::JuntoMcp::from_ledger(ledger.clone());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(web::router(ledger));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(
        "junto serving MCP at http://127.0.0.1:{port}/mcp and channels at \
         http://127.0.0.1:{port}/channels/{{name}} over {}",
        repo.display()
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

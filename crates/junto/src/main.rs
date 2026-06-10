//! junto — the host/app entry point.
//!
//! NOTE: junto is **terminal-less for humans** (CLAUDE.md constraint #2): the
//! constraint is about the *working* surface, not setup plumbing. This binary
//! is the host *process* plus one-time setup commands; `serve` starts the
//! long-running singleton host (docs/adr/0015) that agents (and, later, human
//! surfaces) connect to. Binary/`main` code may use `anyhow` and may
//! `?`-propagate — unlike the library crates.

mod binding;
mod host;
mod init;
mod mcp;
mod render;
mod web;

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use junto_kernel::{ChannelId, Member};
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
    /// Serve the MCP write surface over streamable HTTP (docs/adr/0012) and
    /// the read surface (docs/adr/0013) over every registered home substrate
    /// (docs/adr/0015).
    ///
    /// Agents connect at http://127.0.0.1:<port>/mcp; humans read channels at
    /// http://127.0.0.1:<port>/.
    Serve {
        /// Serve only this repository instead of the machine registry
        /// (single-substrate dev mode).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Port to bind on localhost. 1727: the year Franklin founded the Junto.
        #[arg(long, default_value_t = 1727)]
        port: u16,
    },
    /// Open a channel (docs/adr/0014/0016): mint its id and write the
    /// ChannelOpened genesis entry binding the name, directly into the home
    /// substrate (no running host required).
    Open {
        /// The channel's human-facing name (unique within the home substrate).
        name: String,
        /// The home substrate repo. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// The opener's display name.
        #[arg(long)]
        author_name: String,
        /// The opener's email (the stable identity key).
        #[arg(long)]
        author_email: String,
        /// Declare an existing channel id instead of minting one — the
        /// grandfathering path for records that predate docs/adr/0014.
        #[arg(long)]
        id: Option<String>,
    },
    /// Set a project repo up for junto (docs/adr/0015): register it as a home
    /// substrate, wire the agent harness (.mcp.json + the SessionStart recall
    /// hook), and write the committed channel binding (.junto.toml).
    Init {
        /// The project repo. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// The ambient channel name for the committed binding. Defaults to
        /// the repo's directory name.
        #[arg(long)]
        channel: Option<String>,
        /// Also open the ambient channel (genesis authored by the repo's git
        /// user).
        #[arg(long)]
        open: bool,
    },
    /// Print the briefs of every channel this checkout is bound to
    /// (.junto.toml + .junto.local.toml) — the SessionStart recall hook
    /// (docs/adr/0013). Best-effort: never fails session start.
    Brief {
        /// The checkout directory. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
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
        Command::Open {
            name,
            repo,
            author_name,
            author_email,
            id,
        } => open(name, repo, author_name, author_email, id).await,
        Command::Init {
            repo,
            channel,
            open,
        } => init::run(&repo, channel, open).await,
        Command::Brief { dir } => brief(dir).await,
    }
}

/// Print the briefs of every channel this checkout is bound to. Best-effort by
/// design — a SessionStart hook must never break session start, so failures
/// are notes on stderr and the exit is always success.
async fn brief(dir: PathBuf) -> Result<()> {
    let channels = match binding::bound_channels(&dir) {
        Ok(channels) => channels,
        Err(err) => {
            eprintln!("junto brief: {err:#}");
            return Ok(());
        }
    };
    if channels.is_empty() {
        return Ok(());
    }
    let host = match host::junto_home() {
        Ok(junto_home) => host::Host::from_registry(junto_home),
        Err(err) => {
            eprintln!("junto brief: {err:#}");
            return Ok(());
        }
    };
    for channel in channels {
        match host.resolve(&channel).await {
            Ok(host::Resolution::Resolved { ledger, id, .. }) => {
                match ledger.lock().await.project(&id).await {
                    Ok(view) => {
                        let name = view.name.clone().unwrap_or_else(|| channel.clone());
                        println!("{}", render::brief_markdown(&name, &id, &view));
                    }
                    Err(err) => eprintln!("junto brief: projecting '{channel}': {err}"),
                }
            }
            Ok(host::Resolution::NotFound) => {
                eprintln!("junto brief: bound channel '{channel}' not found (not opened yet?)");
            }
            Ok(host::Resolution::Ambiguous(substrates)) => {
                eprintln!(
                    "junto brief: bound channel '{channel}' is ambiguous across {substrates:?}; \
                     bind by id"
                );
            }
            Err(err) => eprintln!("junto brief: resolving '{channel}': {err:#}"),
        }
    }
    Ok(())
}

/// Run the host until Ctrl-C: the MCP write surface at /mcp, the channel index
/// at /, and the read-only channel pages at /channels/{name} (+ /brief).
async fn serve(repo: Option<PathBuf>, port: u16) -> Result<()> {
    let host = match repo {
        Some(repo) => host::Host::fixed(vec![dunce::canonicalize(&repo)?]),
        None => {
            let junto_home = host::junto_home()?;
            if host::registered_substrates(&junto_home)?.is_empty() {
                bail!(
                    "no home substrates registered under {} — run `junto init` in a repo, \
                     or serve one directly with --repo",
                    junto_home.display()
                );
            }
            host::Host::from_registry(junto_home)
        }
    };

    let handler = mcp::JuntoMcp::new(host.clone());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .merge(web::router(host.clone()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(
        "junto serving MCP at http://127.0.0.1:{port}/mcp and channels at \
         http://127.0.0.1:{port}/ over {:?}",
        host.substrate_paths()?
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Open a channel by writing its genesis straight into the substrate.
async fn open(
    name: String,
    repo: PathBuf,
    author_name: String,
    author_email: String,
    id: Option<String>,
) -> Result<()> {
    let repo = dunce::canonicalize(&repo)?;
    let declared_id = id.map(|raw| raw.parse::<ChannelId>()).transpose()?;
    // A fixed single-substrate host gives us the same open semantics (name
    // uniqueness under the append lock) the MCP tool uses.
    let host = host::Host::fixed(vec![repo.clone()]);
    let opened = host
        .open_channel(
            Some(&repo),
            &name,
            Member::human(&author_name, &author_email),
            declared_id,
        )
        .await?;
    println!(
        "opened channel '{name}' (id {opened}) in {}",
        repo.display()
    );
    Ok(())
}

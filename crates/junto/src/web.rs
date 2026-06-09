//! The host's read-only web routes — the first human surface.
//!
//! Two GET endpoints over the same projection (`docs/adr/0013`):
//! - `/channels/{name}` — HTML for a human reading the channel in a browser
//!   (terminal-less: this, not `git show`, is how a person sees the record).
//! - `/channels/{name}/brief` — the markdown brief; the SessionStart recall
//!   hook curls this into agent context, and anything else that wants the
//!   projection without an MCP handshake can too.
//!
//! Strictly read-only: every write goes through the MCP tools, where authoring
//! identity is explicit.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use junto_kernel::{ChannelId, ChannelView, Ledger};
use junto_substrate_git::GitRefsSubstrate;
use tokio::sync::Mutex;

use crate::render;

/// The one ledger shared by the MCP tools and the web routes.
pub type SharedLedger = Arc<Mutex<Ledger<GitRefsSubstrate>>>;

/// The read-only routes, to be merged into the host's router.
pub fn router(ledger: SharedLedger) -> Router {
    Router::new()
        .route("/channels/{name}", get(channel_page))
        .route("/channels/{name}/brief", get(channel_brief))
        .with_state(ledger)
}

/// Project `name`'s channel, or surface the failure as a 500.
async fn project(ledger: &SharedLedger, name: &str) -> Result<(ChannelId, ChannelView), Response> {
    let id = ChannelId::from_name(name);
    match ledger.lock().await.project(&id).await {
        Ok(view) => Ok((id, view)),
        Err(err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("projection failed: {err}"),
        )
            .into_response()),
    }
}

async fn channel_page(State(ledger): State<SharedLedger>, Path(name): Path<String>) -> Response {
    match project(&ledger, &name).await {
        Ok((id, view)) => Html(render::channel_html(&name, &id, &view)).into_response(),
        Err(response) => response,
    }
}

async fn channel_brief(State(ledger): State<SharedLedger>, Path(name): Path<String>) -> Response {
    match project(&ledger, &name).await {
        Ok((id, view)) => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            render::brief_markdown(&name, &id, &view),
        )
            .into_response(),
        Err(response) => response,
    }
}

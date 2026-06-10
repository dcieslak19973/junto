//! The host's read-only web routes — the first human surface.
//!
//! Three GET endpoints over the same projection (`docs/adr/0013`, `0015`):
//! - `/` — the channel index: every channel across every registered home
//!   substrate (the "one surface" view).
//! - `/channels/{channel}` — HTML for a human reading one channel in a
//!   browser (terminal-less: this, not `git show`, is how a person sees the
//!   record). `{channel}` is a name or a raw channel id.
//! - `/channels/{channel}/brief` — the markdown brief; the SessionStart
//!   recall hook curls this into agent context, and anything else that wants
//!   the projection without an MCP handshake can too.
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
use junto_kernel::{ChannelId, ChannelView};

use crate::host::{Host, Resolution};
use crate::render;

/// The read-only routes, to be merged into the host's router.
pub fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/", get(index_page))
        .route("/channels/{channel}", get(channel_page))
        .route("/channels/{channel}/brief", get(channel_brief))
        .with_state(host)
}

/// Resolve and project a channel reference, or surface the failure as an
/// appropriate HTTP status.
async fn project(host: &Host, channel: &str) -> Result<(ChannelId, ChannelView), Response> {
    let resolution = host
        .resolve(channel)
        .await
        .map_err(|err| internal(format!("resolving '{channel}': {err}")))?;
    let (ledger, id) = match resolution {
        Resolution::Resolved { ledger, id, .. } => (ledger, id),
        Resolution::NotFound => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no channel '{channel}' in any registered substrate"),
            )
                .into_response());
        }
        Resolution::Ambiguous(substrates) => {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "channel name '{channel}' exists in several substrates ({substrates:?}); \
                     address it by id"
                ),
            )
                .into_response());
        }
    };
    let view = ledger
        .lock()
        .await
        .project(&id)
        .await
        .map_err(|err| internal(format!("projection failed: {err}")))?;
    Ok((id, view))
}

fn internal(message: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
}

async fn index_page(State(host): State<Arc<Host>>) -> Response {
    match host.inventory().await {
        Ok(mut summaries) => {
            // Most recently active first — the resumption order.
            summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
            Html(render::index_html(&summaries)).into_response()
        }
        Err(err) => internal(format!("listing channels: {err}")),
    }
}

async fn channel_page(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view)) => {
            let name = view.name.clone().unwrap_or_else(|| channel.clone());
            Html(render::channel_html(&name, &id, &view)).into_response()
        }
        Err(response) => response,
    }
}

async fn channel_brief(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view)) => {
            let name = view.name.clone().unwrap_or_else(|| channel.clone());
            (
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                render::brief_markdown(&name, &id, &view),
            )
                .into_response()
        }
        Err(response) => response,
    }
}

//! The host's web routes — the first human surface.
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
//! And the POSTs — the **human write surface**: verification acts (ratify /
//! park / approve / reject) from the channel page's forms, opening a channel
//! (`/channels`, from the index form or a channel page's contextual
//! open-an-inquiry-here form), and setting a repo up as a home substrate
//! (`/repos` — the terminal-less `junto init`). All authored as the machine
//! user's git identity ([`crate::host::git_user`]) — identity stays claimed
//! (`docs/adr/0012`), this is a default, not an identity system. Recording
//! assertions and proposing actions remain agent (MCP) territory.
//! Each verification act triggers a **best-effort background sync** with
//! `origin` — the page is a terminal-less human's only affordance, so the
//! durable record must not wait for an agent to run `sync_channel`.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Form, Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use junto_kernel::{ChannelId, ChannelView, EntryId, EntryPayload, LedgerEntry, Timestamp};
use serde::{Deserialize, Serialize};

use crate::RedeemOutcome;
use crate::host::{Host, Resolution};
use crate::render;

/// The web routes, to be merged into the host's router.
pub fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/", get(index_page))
        .route("/new", get(new_page))
        .route("/settings", get(settings_page))
        .route("/agents", get(agents_page).post(save_agent))
        .route("/agents/{slug}/delete", post(delete_agent))
        .route("/channels", post(open_channel))
        .route("/repos", post(setup_repo))
        .route("/invites", post(mint_invite))
        .route("/devices/enroll", post(enroll_device))
        .route("/devices/preview", post(preview_enrollment))
        .route("/members", post(redeem_enrollment_endpoint))
        .route("/channels/{channel}", get(channel_page))
        .route("/channels/{channel}/sessions", post(launch_session))
        .route(
            "/channels/{channel}/sessions/{session}/steer",
            post(steer_session),
        )
        .route(
            "/channels/{channel}/sessions/{session}/interrupt",
            post(interrupt_session),
        )
        .route(
            "/channels/{channel}/sessions/{session}/stream",
            get(stream_session),
        )
        .route(
            "/channels/{channel}/artifacts/{artifact}",
            get(view_artifact),
        )
        .route(
            "/channels/{channel}/artifacts/{artifact}/content.json",
            get(artifact_content_json),
        )
        .route("/channels/{channel}/rename", post(rename_channel))
        .route("/channels/{channel}/close", post(close_channel))
        .route("/channels/{channel}/reopen", post(reopen_channel))
        .route("/channels/{channel}/diverge", post(diverge_channel))
        .route("/channels/{channel}/converge", post(converge_channel))
        .route("/channels/{channel}/brief", get(channel_brief))
        .route("/channels/{channel}/view.json", get(channel_view_json))
        .route("/channels/{channel}/keys.json", get(keys_json))
        .route("/channels.json", get(channels_json))
        .route("/lineage.json", get(lineage_json))
        .route("/focus.json", get(focus_json))
        .route("/agents.json", get(agents_json))
        .route("/workspaces.json", get(workspaces_json))
        .route("/substrates.json", get(substrates_json))
        .route("/settings.json", get(settings_json))
        .route("/channels/{channel}/entries/{entry}/{act}", post(verify))
        .route(
            "/channels/{channel}/keys/{grant}/retire",
            post(retire_device),
        )
        .route(
            "/channels/{channel}/members/{email}/revoke",
            post(revoke_member),
        )
        .route(
            "/channels/{channel}/sessions/{session}/live",
            get(crate::live_ws::live_session),
        )
        .with_state(host)
        // Wrap any plain-text error response in a styled page (so a refused
        // act reads as a calm card, not a bare body on a blank page).
        .layer(axum::middleware::map_response(prettify_errors))
}

/// Resolve and project a channel reference, or surface the failure as an
/// appropriate HTTP status. Also yields the home substrate path — the
/// channel page's contextual open-an-inquiry form prefills it.
// Response-as-error is the axum idiom every caller relies on with `?`; the
// Err path is a cold HTTP error, so the 128-byte variant clippy 1.98 flags
// (result_large_err) costs nothing per-request, and boxing it would add
// deref noise at every callsite.
#[allow(clippy::result_large_err)]
pub(crate) async fn project(
    host: &Host,
    channel: &str,
) -> Result<(ChannelId, ChannelView, std::path::PathBuf), Response> {
    let (ledger, id, substrate) = resolve_for_projection(host, channel).await?;
    let view = ledger
        .lock()
        .await
        .project(&id)
        .await
        .map_err(|err| internal(format!("projection failed: {err}")))?;
    Ok((id, view, substrate))
}

/// [`project`], but always re-folds from the substrate
/// (`junto_kernel::Ledger::project_fresh`) instead of `project`'s cached
/// read. The one caller: `crate::live_ws::live_session`'s handshake — see
/// `Ledger::project_fresh`'s doc comment for why a long-running `junto
/// serve` needs this specifically (a `revoke-member`/`retire-device` run
/// in a separate process never invalidates this process's cache) and why
/// every other reader here keeps using the cached [`project`].
#[allow(clippy::result_large_err)]
pub(crate) async fn project_fresh(
    host: &Host,
    channel: &str,
) -> Result<(ChannelId, ChannelView, std::path::PathBuf), Response> {
    let (ledger, id, substrate) = resolve_for_projection(host, channel).await?;
    let view = ledger
        .lock()
        .await
        .project_fresh(&id)
        .await
        .map_err(|err| internal(format!("projection failed: {err}")))?;
    Ok((id, view, substrate))
}

// Same Response-as-error idiom, same cold Err path, and both callers above
// already carry this allow — boxing here would only add a deref at each of
// them. See the rationale above `project`.
#[allow(clippy::result_large_err)]
async fn resolve_for_projection(
    host: &Host,
    channel: &str,
) -> Result<(crate::host::SharedLedger, ChannelId, std::path::PathBuf), Response> {
    let resolution = host
        .resolve(channel)
        .await
        .map_err(|err| internal(format!("resolving '{channel}': {err}")))?;
    match resolution {
        Resolution::Resolved {
            ledger,
            id,
            substrate,
        } => Ok((ledger, id, substrate)),
        Resolution::NotFound => Err((
            StatusCode::NOT_FOUND,
            format!("no channel '{channel}' in any registered substrate"),
        )
            .into_response()),
        Resolution::Ambiguous(substrates) => Err((
            StatusCode::CONFLICT,
            format!(
                "channel name '{channel}' exists in several substrates ({substrates:?}); \
                 address it by id"
            ),
        )
            .into_response()),
    }
}

fn internal(message: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
}

/// Build a JSON refusal for one of the six identity endpoints (finding
/// 1, final fix wave): `prettify_errors` below already passes
/// `application/json` through untouched, and the native GUI's
/// `describe_failed_response` (`crates/junto-iced/src/main.rs`) already
/// extracts a `message` field from a JSON body — so every refusal these
/// endpoints build with this (instead of the shared `(StatusCode,
/// String)` idiom every HTML-rendering route here still uses) renders
/// verbatim in the GUI instead of collapsing to a fixed "see the host
/// log" sentence the desktop app has no host log to back.
fn identity_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        axum::Json(serde_json::json!({ "message": message.into() })),
    )
        .into_response()
}

/// Rewrite an already-built plain-text error [`Response`] — the shared
/// `project`/`resolve_for_projection` idiom, which the HTML-rendering
/// channel routes also use and which `prettify_errors` turns into a
/// styled page for them — into [`identity_error`]'s same `{"message":
/// …}` envelope. For the identity endpoints that resolve a channel
/// through those shared helpers (`mint_invite` via [`project_fresh`],
/// [`keys_json`]): the rewrite happens at THEIR call sites, never inside
/// the shared helpers themselves, so the HTML routes sharing those
/// helpers keep their styled-page behavior untouched.
async fn as_identity_error(response: Response) -> Response {
    let status = response.status();
    let message = match axum::body::to_bytes(response.into_body(), 64 * 1024).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => "internal error".to_string(),
    };
    identity_error(status, message)
}

/// Response layer: turn any error-status plain-text response (the handlers'
/// `(StatusCode, message)` returns) into a styled error page, so the human
/// surface never shows a bare body on a blank page. Already-HTML and
/// already-JSON responses pass through untouched — a JSON endpoint's error
/// body (e.g. `POST /members`'s 409 `{"outcomes":[…]}`) is structured data a
/// non-HTML caller parses, not a page to prettify; rewriting it here would
/// silently discard the per-channel truth the brief requires that body to
/// carry.
async fn prettify_errors(response: Response) -> Response {
    let status = response.status();
    if !status.is_client_error() && !status.is_server_error() {
        return response;
    }
    if let Some(content_type) = response.headers().get(header::CONTENT_TYPE)
        && content_type.to_str().is_ok_and(|value| {
            value.starts_with("text/html") || value.starts_with("application/json")
        })
    {
        return response;
    }
    let title = match status {
        StatusCode::FORBIDDEN => "You can’t do that here",
        StatusCode::BAD_REQUEST => "That needs a small fix",
        StatusCode::CONFLICT => "That conflicts with the record",
        StatusCode::NOT_FOUND => "Not found",
        _ => "Something went wrong",
    };
    let message = match axum::body::to_bytes(response.into_body(), 64 * 1024).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => "Unknown error.".to_string(),
    };
    (status, Html(render::error_page(title, &message))).into_response()
}

/// Index query: `?w=` scopes the page to one workspace (home substrate). An
/// unrecognized `expanded` flag (the strip's walk-back link) is ignored for now.
#[derive(Debug, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    w: String,
}

async fn index_page(
    State(host): State<Arc<Host>>,
    axum::extract::Query(query): axum::extract::Query<IndexQuery>,
) -> Response {
    let substrates = host.substrate_paths().unwrap_or_default();
    // The active workspace: the requested one, else the first registered.
    let active = if query.w.trim().is_empty() {
        substrates.first().cloned()
    } else {
        Some(std::path::PathBuf::from(query.w.trim()))
    };
    // One projection sweep yields both the strip and the board.
    match host.overview().await {
        Ok((summaries, attention)) => {
            let Some(active) = active else {
                // No registered substrate at all: render an empty shell.
                let empty = render::LineageModel {
                    mainline: render::Track::empty(),
                    branches: Vec::new(),
                };
                return Html(render::new_index_html(
                    &substrates,
                    std::path::Path::new(""),
                    &empty,
                    &[],
                    &[],
                    None,
                ))
                .into_response();
            };
            // Scope to the active workspace: its channels (for the strip) and
            // their attention groups (for the board).
            let scoped: Vec<_> = summaries
                .iter()
                .filter(|summary| summary.substrate == active)
                .cloned()
                .collect();
            let in_workspace: std::collections::HashSet<ChannelId> =
                scoped.iter().map(|summary| summary.id).collect();
            let scoped_attention: Vec<_> = attention
                .iter()
                .filter(|group| in_workspace.contains(&group.channel))
                .cloned()
                .collect();
            let model = render::LineageModel::from_summaries(&scoped, &active);
            let identity = crate::host::git_user(&active).ok();
            let who = identity.as_ref().map(|member| member.display_name.as_str());
            Html(render::new_index_html(
                &substrates,
                &active,
                &model,
                &scoped,
                &scoped_attention,
                who,
            ))
            .into_response()
        }
        Err(err) => internal(format!("listing channels: {err}")),
    }
}

/// The "/new" page behind the sidebar's "+ new" menu: open a channel, set up
/// a repo. The substrates feed the open form's picker (shown only when
/// several are registered).
async fn new_page(State(host): State<Arc<Host>>) -> Response {
    let mut nav = host.inventory().await.unwrap_or_default();
    nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
    let substrates = host.substrate_paths().unwrap_or_default();
    Html(render::new_html(&nav, &substrates)).into_response()
}

/// The "/settings" page behind the sidebar's ⚙: machine-local preferences and
/// status — how the harness runs (`docs/adr/0023`/`0024`), the registered
/// substrates, and the identity human-surface acts author as. Read-only.
async fn settings_page(State(host): State<Arc<Host>>) -> Response {
    let mut nav = host.inventory().await.unwrap_or_default();
    nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
    let substrates = host.substrate_paths().unwrap_or_default();
    let status = crate::launch::harness_status();
    // Who human-surface acts author as: the git user of the first substrate.
    let identity = substrates
        .first()
        .and_then(|repo| crate::host::git_user(repo).ok());
    let identity_pair = identity
        .as_ref()
        .map(|member| (member.display_name.as_str(), member.email.as_str()));
    Html(render::settings_html(
        &nav,
        &substrates,
        &status,
        identity_pair,
        env!("CARGO_PKG_VERSION"),
        "http://127.0.0.1:1727",
    ))
    .into_response()
}

/// The "/agents" page behind the sidebar's ✦: create, edit, and delete the
/// reusable agent agents the launch picker offers
/// (`docs/superpowers/specs/2026-06-13-agent-personas-design.md`).
async fn agents_page(State(host): State<Arc<Host>>) -> Response {
    let mut nav = host.inventory().await.unwrap_or_default();
    nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    match crate::agent::all_agents(&junto_home) {
        Ok(agents) => Html(render::agents_html(&nav, &agents)).into_response(),
        Err(err) => internal(format!("reading agents: {err}")),
    }
}

/// Lowercase, hyphenate, and trim a name into a stable slug for a new agent.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// The first value submitted for `key`, or `""`.
fn field<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// Pair the repeated `mcp_name`/`mcp_url` fields into MCP servers, in order,
/// dropping any row where either side is blank (the trailing add-row, a row
/// the user left empty). The form renders one `mcp_name` and one `mcp_url`
/// per row, so the i-th of each belong together.
fn parse_mcp_rows(pairs: &[(String, String)]) -> Vec<crate::agent::McpServer> {
    let names = pairs.iter().filter(|(k, _)| k == "mcp_name");
    let urls = pairs.iter().filter(|(k, _)| k == "mcp_url");
    names
        .zip(urls)
        .filter_map(|((_, name), (_, url))| {
            let (name, url) = (name.trim(), url.trim());
            (!name.is_empty() && !url.is_empty()).then(|| crate::agent::McpServer {
                name: name.to_string(),
                url: url.to_string(),
            })
        })
        .collect()
}

/// Every value submitted for `key`, in order, trimmed and non-empty. Used for
/// the repeated `skill` checkboxes and `plugin_path` rows.
fn all_fields(pairs: &[(String, String)], key: &str) -> Vec<String> {
    pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

/// Save (create or update) an Agent from the agents-page form. The body is
/// read as raw key/value pairs so the repeated rows (MCP servers, plugins) and
/// the `skill` checkboxes survive — axum's typed `Form` can't collect duplicate
/// keys into a list.
async fn save_agent(
    State(_host): State<Arc<Host>>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let name = field(&pairs, "name").trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "a name is required").into_response();
    }
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    // On edit the slug is fixed; on create derive it from the name.
    let form_slug = field(&pairs, "slug").trim();
    let slug = if form_slug.is_empty() {
        slugify(&name)
    } else {
        form_slug.to_string()
    };
    if slug.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "the name has no slug-able characters — give it a plain-text name",
        )
            .into_response();
    }
    // Preserve an existing agent's email so editing a stock agent keeps the
    // harness identity; a brand-new custom agent gets its own.
    let email = match crate::agent::agent_by_slug(&junto_home, &slug) {
        Ok(Some(existing)) => existing.email,
        Ok(None) => format!("{slug}@junto.local"),
        Err(err) => return internal(format!("reading agents: {err}")),
    };
    let trimmed = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    let agent = crate::agent::Agent {
        slug,
        name,
        harness: crate::launch::harness_by_id(field(&pairs, "harness").trim())
            .id
            .to_string(),
        email,
        role: trimmed(field(&pairs, "role")),
        model: trimmed(field(&pairs, "model")),
        mcp_servers: parse_mcp_rows(&pairs),
        skills: all_fields(&pairs, "skill"),
        plugins: all_fields(&pairs, "plugin_path"),
    };
    match crate::agent::save_agent(&junto_home, agent) {
        Ok(()) => Redirect::to("/agents").into_response(),
        Err(err) => internal(format!("saving agent: {err}")),
    }
}

/// Delete an Agent by slug from the agents page.
async fn delete_agent(State(_host): State<Arc<Host>>, Path(slug): Path<String>) -> Response {
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    match crate::agent::delete_agent(&junto_home, &slug) {
        Ok(()) => Redirect::to("/agents").into_response(),
        Err(err) => internal(format!("deleting agent: {err}")),
    }
}

/// The form body for opening a channel from the index page.
#[derive(Debug, Deserialize)]
struct OpenChannelForm {
    /// The channel's name — a label, unique within its home substrate
    /// (`docs/adr/0014`).
    name: String,
    /// The home substrate repo path; may be empty when the host serves
    /// exactly one.
    #[serde(default)]
    repo: String,
}

/// Open a channel from the index page's form: the human-surface counterpart
/// of the `open_channel` MCP tool. The founder is the substrate's git user —
/// the host derives the author, same as verification acts (`docs/adr/0021`).
async fn open_channel(
    State(host): State<Arc<Host>>,
    Form(form): Form<OpenChannelForm>,
) -> Response {
    let name = form.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "a channel needs a name").into_response();
    }
    let substrates = match host.substrate_paths() {
        Ok(substrates) => substrates,
        Err(err) => return internal(format!("listing substrates: {err}")),
    };
    let repo = if form.repo.trim().is_empty() {
        match substrates.as_slice() {
            [only] => only.clone(),
            [] => {
                return internal(
                    "no registered home substrates (run `junto init` in a repo first)".into(),
                );
            }
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    "several substrates are registered — pick the home substrate in the form",
                )
                    .into_response();
            }
        }
    } else {
        std::path::PathBuf::from(form.repo.trim())
    };
    let founder = match crate::host::git_user(&repo) {
        Ok(founder) => founder,
        Err(err) => {
            return internal(format!(
                "no founder identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    match host.open_channel(Some(&repo), name, founder, None).await {
        // Id-addressed: ids are URL-safe, names may not be.
        Ok(opened) => Redirect::to(&format!("/channels/{}", opened.id)).into_response(),
        // Name taken, unregistered substrate, … — the message says which.
        Err(err) => (StatusCode::CONFLICT, format!("{err:#}")).into_response(),
    }
}

/// The form body for setting a repo up as a home substrate.
#[derive(Debug, Deserialize)]
struct SetupRepoForm {
    /// Filesystem path to a git repository on this machine.
    path: String,
    /// The ambient channel's name; empty defaults to the repo's directory
    /// name (mirroring `junto init`).
    #[serde(default)]
    channel: String,
}

/// Set a repo up from the index page — the terminal-less `junto init`
/// (constraint #2: the host runs as the machine user, so it can do
/// everything the CLI did): register the substrate, wire the agent harness,
/// bind and **open** the ambient channel, then land on its page. The
/// agent-membership grant stays on `junto add-member` for now.
async fn setup_repo(State(host): State<Arc<Host>>, Form(form): Form<SetupRepoForm>) -> Response {
    let path = std::path::PathBuf::from(form.path.trim());
    if form.path.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "a repo path is required").into_response();
    }
    let channel = match form.channel.trim() {
        "" => None,
        name => Some(name.to_string()),
    };
    if let Err(err) = crate::init::run(&path, channel.clone(), true, None).await {
        return (StatusCode::BAD_REQUEST, format!("{err:#}")).into_response();
    }
    // Land on the ambient channel's page (init derived its name from the
    // directory when none was given — re-derive the same way).
    let ambient = match channel {
        Some(name) => name,
        None => match path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        {
            Some(name) => name,
            None => return Redirect::to("/").into_response(),
        },
    };
    match host.resolve(&ambient).await {
        Ok(Resolution::Resolved { id, .. }) => {
            Redirect::to(&format!("/channels/{id}")).into_response()
        }
        // Ambiguous (the name exists elsewhere too) or anything unexpected:
        // the index shows the new substrate either way.
        _ => Redirect::to("/").into_response(),
    }
}

async fn channel_page(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view, substrate)) => {
            let name = view.name.clone().unwrap_or_else(|| channel.clone());
            // The sidebar: every channel, most recently active first —
            // best-effort, an empty nav never blocks the page itself.
            let mut nav = host.inventory().await.unwrap_or_default();
            nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
            // The remembered workspace prefills the start-work form
            // (docs/adr/0023); best-effort, like the nav.
            let workspace = crate::host::junto_home()
                .ok()
                .and_then(|home| crate::launch::workspace_for(&home, &id).ok().flatten());
            Html(render::channel_html(
                &nav,
                &name,
                &id,
                &view,
                &substrate,
                workspace.as_deref(),
            ))
            .into_response()
        }
        Err(response) => response,
    }
}

/// The form body for launching an Agent Session (`docs/adr/0023`).
#[derive(Debug, Deserialize)]
struct LaunchForm {
    /// What the agent should do — becomes the session's intent and the
    /// harness prompt.
    intent: String,
    /// The workspace repo; empty falls back to the remembered mapping.
    #[serde(default)]
    workspace: String,
    /// Which agent runs it (its slug); empty/unknown → the default agent
    /// (`docs/superpowers/specs/2026-06-13-agent-personas-design.md`).
    #[serde(default)]
    agent: String,
    /// `"outcome"` runs the code-PR push-gate (the verify/Grader loop,
    /// `docs/adr/0025`); anything else runs a single turn.
    #[serde(default)]
    mode: String,
}

/// Launch an Agent Session from the channel page: resolve the workspace
/// (remembering a newly typed one), check the agent's member is in the
/// Party, and spawn the first turn in the background.
async fn launch_session(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<LaunchForm>,
) -> Response {
    let intent = form.intent.trim().to_string();
    if intent.is_empty() {
        return (StatusCode::BAD_REQUEST, "an intent is required").into_response();
    }
    let (id, view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    if view.closed {
        return (
            StatusCode::CONFLICT,
            "this channel is closed — reopen it before starting work",
        )
            .into_response();
    }
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    // The session's author is the agent's member (docs/adr/0020); its entries
    // only project once it is in the Party (docs/adr/0017). Rather than reject
    // a launch in a fresh channel, bring the agent in: if the human at the
    // keyboard founded this channel, auto-grant membership (a founder-authored
    // MemberAdded) so the agent joins and starts work in one motion — the
    // grant is recorded, not hidden. A non-founder can't grant: they get
    // add_member's error naming who can (docs/adr/0017).
    // One agent per channel (docs/adr/0024): if an Agent already serves this
    // channel (its member is in the Party), reuse it — the picker only chooses
    // the agent the first time. Otherwise the form's selection becomes the
    // channel's agent (the default agent for an empty/unknown slug),
    // granted below.
    let agent = match crate::agent::channel_agent(&junto_home, &view.party) {
        Ok(Some(established)) => established,
        Ok(None) => match resolve_form_agent(&junto_home, form.agent.trim()) {
            Ok(agent) => agent,
            Err(err) => return internal(format!("reading agents: {err}")),
        },
        Err(err) => return internal(format!("reading agents: {err}")),
    };
    let agent_member = agent.member();
    let agent_is_member = view.party.iter().any(|m| m.email == agent_member.email);
    if !view.party.is_empty() && !agent_is_member {
        let granter = match crate::host::git_user(&substrate) {
            Ok(granter) => granter,
            Err(err) => {
                return internal(format!(
                    "no author identity: {err} (set git config user.name / user.email)"
                ));
            }
        };
        if let Err(err) = host
            .add_member(&channel, &granter, agent_member, None, None)
            .await
        {
            return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
        }
    }
    let workspace = if form.workspace.trim().is_empty() {
        match crate::launch::workspace_for(&junto_home, &id) {
            Ok(Some(workspace)) => workspace,
            Ok(None) => {
                return (
                    StatusCode::BAD_REQUEST,
                    "no workspace is remembered for this channel — fill in the workspace \
                     repo path (it will be remembered)",
                )
                    .into_response();
            }
            Err(err) => return internal(format!("reading workspaces: {err}")),
        }
    } else {
        let typed = std::path::PathBuf::from(form.workspace.trim());
        if let Err(err) = crate::launch::remember_workspace(&junto_home, &id, &typed) {
            return (StatusCode::BAD_REQUEST, format!("{err:#}")).into_response();
        }
        match crate::launch::workspace_for(&junto_home, &id) {
            Ok(Some(workspace)) => workspace,
            _ => return internal("workspace vanished after remembering".into()),
        }
    };
    // "outcome" runs the code-PR push-gate (the verify/Grader loop, docs/adr/0025);
    // otherwise a single turn (docs/adr/0023).
    let launched = if form.mode.trim() == "outcome" {
        crate::launch::launch_outcome(host.clone(), id, channel.clone(), workspace, intent, agent)
            .await
    } else {
        crate::launch::launch(host.clone(), id, channel.clone(), workspace, intent, agent).await
    };
    match launched {
        Ok(_session) => Redirect::to(&format!("/channels/{id}")).into_response(),
        Err(err) => internal(format!("launch failed: {err:#}")),
    }
}

/// Resolve the agent a launch form selected: the named slug, or the default
/// agent (the first, the stock entry for the default harness) when the field
/// is empty or names no known agent.
fn resolve_form_agent(
    junto_home: &std::path::Path,
    slug: &str,
) -> anyhow::Result<crate::agent::Agent> {
    if !slug.is_empty()
        && let Some(agent) = crate::agent::agent_by_slug(junto_home, slug)?
    {
        return Ok(agent);
    }
    let mut all = crate::agent::all_agents(junto_home)?;
    if all.is_empty() {
        anyhow::bail!("no agents available");
    }
    Ok(all.remove(0))
}

/// The form body for steering a session.
#[derive(Debug, Deserialize)]
struct SteerForm {
    /// The follow-up instruction — recorded as a `SessionUpdated` note, then
    /// transported via `--resume` (docs/adr/0023).
    message: String,
}

/// Steer an existing session from its card.
async fn steer_session(
    State(host): State<Arc<Host>>,
    Path((channel, session)): Path<(String, String)>,
    Form(form): Form<SteerForm>,
) -> Response {
    let message = form.message.trim().to_string();
    if message.is_empty() {
        return (StatusCode::BAD_REQUEST, "a steer message is required").into_response();
    }
    let Ok(session) = session.parse::<EntryId>() else {
        return (
            StatusCode::BAD_REQUEST,
            format!("'{session}' is not a session id"),
        )
            .into_response();
    };
    let (id, view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    if view.session(&session).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            format!("{session} is not an agent session in this channel"),
        )
            .into_response();
    }
    // The steer note is authored by the human at the keyboard (the record
    // keeps who steered); membership checked like every human-surface act.
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    if let Err(err) = host.authorize_human_write(&view, &author) {
        return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
    }
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    let workspace = match crate::launch::workspace_for(&junto_home, &id) {
        Ok(Some(workspace)) => workspace,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                "no workspace is remembered for this channel on this machine",
            )
                .into_response();
        }
        Err(err) => return internal(format!("reading workspaces: {err}")),
    };
    // One steer box, routed on liveness: a running turn is steered in place over
    // the control channel; a turn that has already landed is resumed (docs/adr/
    // 0032).
    match crate::launch::steer_live(
        host.clone(),
        id,
        channel.clone(),
        session,
        author.clone(),
        message.clone(),
    )
    .await
    {
        Ok(()) => Redirect::to(&format!("/channels/{id}")).into_response(),
        Err(crate::launch::NotLive) => match crate::launch::steer(
            host.clone(),
            id,
            channel.clone(),
            workspace,
            session,
            author,
            message,
        )
        .await
        {
            Ok(()) => Redirect::to(&format!("/channels/{id}")).into_response(),
            Err(err) => (StatusCode::BAD_REQUEST, format!("{err:#}")).into_response(),
        },
    }
}

/// Interrupt a running session's current turn from its live card. Delivers a
/// bare `Interrupt` over the control channel; the turn ends (interrupted) and
/// the card reloads to the landed outcome. `Err(NotLive)` means no turn is
/// currently running for the session.
async fn interrupt_session(
    State(host): State<Arc<Host>>,
    Path((channel, session)): Path<(String, String)>,
) -> Response {
    let Ok(session) = session.parse::<EntryId>() else {
        return (
            StatusCode::BAD_REQUEST,
            format!("'{session}' is not a session id"),
        )
            .into_response();
    };
    let (id, view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    if view.session(&session).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            format!("{session} is not an agent session in this channel"),
        )
            .into_response();
    }
    // Membership checked like every human-surface act.
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    if let Err(err) = host.authorize_human_write(&view, &author) {
        return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
    }
    match host
        .live()
        .control(session, crate::launch::TurnControl::Interrupt)
    {
        Ok(()) => Redirect::to(&format!("/channels/{id}")).into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "no running turn to interrupt").into_response(),
    }
}

/// Stream a running session's live progress as Server-Sent Events
/// (`docs/adr/0023`). The card's `EventSource` subscribes; the turn publishes
/// to the in-memory feed. Read-only — steering is a separate recorded POST, so
/// SSE (server→browser) fits, no WebSocket needed.
///
/// Each progress line is an SSE `live` event carrying the JSON `LiveEvent`.
/// When the turn ends (the feed's sender drops) — or if no feed is running —
/// an `end` event tells the client to stop (no auto-reconnect) and reload to
/// the now-persisted memo + diff. The feed itself is never the record.
async fn stream_session(
    State(host): State<Arc<Host>>,
    Path((_channel, session)): Path<(String, String)>,
) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};

    let Ok(session) = session.parse::<EntryId>() else {
        return (StatusCode::BAD_REQUEST, "not a session id").into_response();
    };
    let subscription = host.live().subscribe(session);
    let stream = async_stream::stream! {
        if let Some((buffer, mut receiver)) = subscription {
            for event in buffer {
                if let Ok(sse) = Event::default().event("live").json_data(&event) {
                    yield Ok::<_, std::convert::Infallible>(sse);
                }
            }
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if let Ok(sse) = Event::default().event("live").json_data(&event) {
                            yield Ok(sse);
                        }
                    }
                    // A slow watcher that fell behind: keep going from the tail.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    // The turn ended (sender dropped): stop.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        // Whether or not a feed was live, end the stream so the client closes
        // (no reconnect) and reloads to the persisted outcome.
        yield Ok(Event::default().event("end").data("done"));
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Serve an artifact's full content (`docs/adr/0020`/`0023`): the memo or diff
/// the card only snippets inline. Artifact content lives machine-local under
/// `~/.junto/artifacts/` (never the ledger), referenced by a `file://` URI —
/// which the desktop webview can't open, so the human surface serves it here.
///
/// The URI is taken from the artifact entry, but **not trusted**: an entry can
/// arrive by sync carrying any path, so the resolved file must sit under this
/// machine's artifacts root before we read it.
async fn view_artifact(
    State(host): State<Arc<Host>>,
    Path((channel, artifact)): Path<(String, String)>,
) -> Response {
    let (content, format) = match resolve_artifact_content(&host, &channel, &artifact).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    // The card's `<details>` lazy-loads this inline. A memo is the agent's
    // prose (rendered as sanitized CommonMark); a diff gets per-line colour;
    // everything else stays verbatim as text.
    let html =
        |body: String| ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    match format {
        render::ArtifactFormat::Markdown => html(render::render_markdown(&content)),
        render::ArtifactFormat::Diff => html(render::render_diff(&content)),
        render::ArtifactFormat::Raw => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            content,
        )
            .into_response(),
    }
}

/// An artifact's **raw content + format** as JSON — for a non-HTML surface (the
/// native Iced app) to render itself (diff colouring, etc.). Same content and
/// security checks as [`view_artifact`], just not pre-rendered to HTML.
async fn artifact_content_json(
    State(host): State<Arc<Host>>,
    Path((channel, artifact)): Path<(String, String)>,
) -> Response {
    let (content, format) = match resolve_artifact_content(&host, &channel, &artifact).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    #[derive(Serialize)]
    struct Out {
        format: &'static str,
        content: String,
    }
    let format = match format {
        render::ArtifactFormat::Markdown => "markdown",
        render::ArtifactFormat::Diff => "diff",
        render::ArtifactFormat::Raw => "raw",
    };
    axum::Json(Out { format, content }).into_response()
}

/// Resolve an artifact entry to its stored content + presentation format,
/// enforcing that the file sits under this machine's artifacts root (an entry
/// can arrive by sync carrying any path, so the resolved file is not trusted
/// until checked). Shared by the HTML and JSON artifact endpoints.
// Response-as-error, cold path — same reasoning as `project` above.
#[allow(clippy::result_large_err)]
async fn resolve_artifact_content(
    host: &Arc<Host>,
    channel: &str,
    artifact: &str,
) -> Result<(String, render::ArtifactFormat), Response> {
    let Ok(artifact_id) = artifact.parse::<EntryId>() else {
        return Err((StatusCode::BAD_REQUEST, "not an artifact id").into_response());
    };
    let (_id, view, _substrate) = project(host, channel).await?;
    let Some(entry) = view.entries.iter().find(|e| e.id == artifact_id) else {
        return Err((StatusCode::NOT_FOUND, "no such artifact in this channel").into_response());
    };
    let EntryPayload::ArtifactAttached {
        kind, provenance, ..
    } = &entry.payload
    else {
        return Err((StatusCode::BAD_REQUEST, "that entry is not an artifact").into_response());
    };
    let Some(file) = provenance.first() else {
        return Err((StatusCode::NOT_FOUND, "artifact has no stored content").into_response());
    };
    let Some(path) = file_uri_to_path(file.uri.as_str()) else {
        return Err((StatusCode::BAD_REQUEST, "artifact is not a local file").into_response());
    };
    // Defense in depth: only ever read under this machine's artifacts root.
    let artifacts_root = match crate::host::junto_home() {
        Ok(home) => home.join("artifacts"),
        Err(err) => return Err(internal(format!("no junto home: {err}"))),
    };
    let (canon_path, canon_root) = match (
        dunce::canonicalize(&path),
        dunce::canonicalize(&artifacts_root),
    ) {
        (Ok(p), Ok(root)) => (p, root),
        _ => {
            return Err((
                StatusCode::NOT_FOUND,
                "artifact content is not on this machine",
            )
                .into_response());
        }
    };
    if !canon_path.starts_with(&canon_root) {
        return Err((StatusCode::FORBIDDEN, "artifact path is outside the store").into_response());
    }
    let content = match std::fs::read_to_string(&canon_path) {
        Ok(content) => content,
        Err(err) => {
            return Err((StatusCode::NOT_FOUND, format!("reading artifact: {err}")).into_response());
        }
    };
    Ok((content, render::artifact_format(kind)))
}

/// Turn a `file://` URI (as `store_artifact` writes it) back into a path.
/// Lenient about the Windows `file:///C:/…` vs POSIX `file:////home/…` forms.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Windows: "/C:/Users/…" → drop the leading slash before the drive.
    // POSIX:   "//home/…"     → drop one slash, leaving "/home/…".
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    Some(std::path::PathBuf::from(rest))
}

/// The form body for renaming a channel.
#[derive(Debug, Deserialize)]
struct RenameForm {
    /// The new name — a label, unique within the home substrate
    /// (`docs/adr/0014`).
    name: String,
    /// Why the rename. A rationale, not a checkbox.
    rationale: String,
}

/// Rename a channel: append a [`EntryPayload::Correction`] targeting the
/// `ChannelOpened` genesis — the corrective-entry rename ADR 0016 anticipated,
/// not mutable metadata. The projection resolves the current name as
/// "genesis unless corrected, last applicable wins"; links stay id-addressed,
/// so nothing breaks.
async fn rename_channel(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<RenameForm>,
) -> Response {
    let new_name = form.name.trim().to_string();
    if new_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "a channel needs a name").into_response();
    }
    if new_name.parse::<ChannelId>().is_ok() {
        return (
            StatusCode::BAD_REQUEST,
            "a channel name must not look like a channel id",
        )
            .into_response();
    }
    let rationale = form.rationale.trim().to_string();
    if rationale.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "a rationale is required — it's a rationale, not a checkbox",
        )
            .into_response();
    }

    let (id, view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    // Name uniqueness within the home substrate, same rule open_channel
    // enforces (docs/adr/0014: names are substrate-scoped labels).
    let taken = host.inventory().await.unwrap_or_default().iter().any(|s| {
        s.substrate == substrate && s.id != id && s.name.as_deref() == Some(new_name.as_str())
    });
    if taken {
        return (
            StatusCode::CONFLICT,
            format!("a channel named '{new_name}' already exists in this substrate"),
        )
            .into_response();
    }
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    if let Err(err) = host.authorize_human_write(&view, &author) {
        return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
    }
    let Some(genesis) = view
        .entries
        .iter()
        .find(|entry| matches!(entry.payload, EntryPayload::ChannelOpened { .. }))
        .map(|entry| entry.id)
    else {
        return (
            StatusCode::CONFLICT,
            "this channel has no genesis entry (pre-0014 record) — it cannot be renamed",
        )
            .into_response();
    };

    let ledger = match host.ledger_for(&substrate).await {
        Ok(ledger) => ledger,
        Err(err) => return internal(format!("opening the ledger: {err}")),
    };
    let mut entry = LedgerEntry {
        signature: None,
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload: EntryPayload::Correction {
            target: genesis,
            statement: new_name,
            rationale,
        },
    };
    host.sign_entry(&mut entry);
    if let Err(err) = ledger.lock().await.append(entry).await {
        return internal(format!("append failed: {err}"));
    }
    // Best-effort background sync, same as verification acts.
    let repo = substrate.clone();
    tokio::spawn(async move {
        let mut fresh = junto_substrate_git::GitRefsSubstrate::open(repo);
        if let Err(err) = fresh.sync("origin", &id).await {
            tracing::warn!("auto-sync of channel {id} after a rename failed: {err:#}");
        }
    });
    Redirect::to(&format!("/channels/{id}")).into_response()
}

/// The form body for closing or reopening a channel: just the rationale.
#[derive(Debug, Deserialize)]
struct LifecycleForm {
    /// Why the channel closes/reopens. A rationale, not a checkbox.
    rationale: String,
}

/// Close a channel (`docs/adr/0022`): append a `ChannelClosed` lifecycle
/// entry. The record stays; the channel leaves the working set.
async fn close_channel(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<LifecycleForm>,
) -> Response {
    lifecycle_act(&host, &channel, form, true).await
}

/// Reopen a closed channel (`docs/adr/0022`): append a `ChannelReopened`
/// lifecycle entry — last applicable wins.
async fn reopen_channel(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<LifecycleForm>,
) -> Response {
    lifecycle_act(&host, &channel, form, false).await
}

/// The shared close/reopen path: author from git config, membership checked,
/// the act appended, background sync, back to the channel page.
async fn lifecycle_act(host: &Host, channel: &str, form: LifecycleForm, close: bool) -> Response {
    let rationale = form.rationale.trim().to_string();
    if rationale.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "a rationale is required — it's a rationale, not a checkbox",
        )
            .into_response();
    }
    let (id, view, substrate) = match project(host, channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    if view.closed == close {
        return (
            StatusCode::CONFLICT,
            format!(
                "this channel is already {}",
                if close { "closed" } else { "open" }
            ),
        )
            .into_response();
    }
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    if let Err(err) = host.authorize_human_write(&view, &author) {
        return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
    }
    let payload = if close {
        EntryPayload::ChannelClosed { rationale }
    } else {
        EntryPayload::ChannelReopened { rationale }
    };
    let ledger = match host.ledger_for(&substrate).await {
        Ok(ledger) => ledger,
        Err(err) => return internal(format!("opening the ledger: {err}")),
    };
    let mut entry = LedgerEntry {
        signature: None,
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload,
    };
    host.sign_entry(&mut entry);
    if let Err(err) = ledger.lock().await.append(entry).await {
        return internal(format!("append failed: {err}"));
    }
    let repo = substrate.clone();
    tokio::spawn(async move {
        let mut fresh = junto_substrate_git::GitRefsSubstrate::open(repo);
        if let Err(err) = fresh.sync("origin", &id).await {
            tracing::warn!("auto-sync of channel {id} after a lifecycle act failed: {err:#}");
        }
    });
    Redirect::to(&format!("/channels/{id}")).into_response()
}

/// Spawn a best-effort background sync of one channel after a write — so the
/// durable record leaves this machine without blocking the redirect.
fn spawn_channel_sync(substrate: std::path::PathBuf, id: ChannelId) {
    tokio::spawn(async move {
        let mut fresh = junto_substrate_git::GitRefsSubstrate::open(substrate);
        if let Err(err) = fresh.sync("origin", &id).await {
            tracing::warn!("auto-sync of channel {id} after a lineage act failed: {err:#}");
        }
    });
}

/// Map a lineage-op error to a styled response: a membership refusal is a
/// FORBIDDEN ("you can't do that here"), everything else (name taken, open
/// gates, no such target) is a correctable BAD_REQUEST.
fn lineage_error(err: anyhow::Error) -> Response {
    let message = format!("{err:#}");
    let status = if message.contains("member") {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, message).into_response()
}

/// Start a side-quest from the channel page (`docs/adr/0027`): open a child
/// channel off this one and record the divergence edge. Author from git config,
/// membership-checked (no member code on the human surface, `docs/adr/0021`).
async fn diverge_channel(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<DivergeForm>,
) -> Response {
    let child_name = form.child_name.trim().to_string();
    if child_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "a side-quest needs a name").into_response();
    }
    let (parent_id, _view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    match host
        .diverge(
            &channel,
            &child_name,
            None,
            author,
            crate::host::WriteAuth::Human,
        )
        .await
    {
        Ok(child) => {
            spawn_channel_sync(substrate.clone(), parent_id);
            spawn_channel_sync(substrate, child.id);
            Redirect::to(&format!("/channels/{}", child.id)).into_response()
        }
        Err(err) => lineage_error(err),
    }
}

/// Converge this channel into another from the channel page (`docs/adr/0027`):
/// record the convergence edge and close the source. Membership-checked.
async fn converge_channel(
    State(host): State<Arc<Host>>,
    Path(channel): Path<String>,
    Form(form): Form<ConvergeForm>,
) -> Response {
    let target = form.target.trim().to_string();
    let rationale = form.rationale.trim().to_string();
    if target.is_empty() || rationale.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "converge needs a target channel and a rationale",
        )
            .into_response();
    }
    let (source_id, _view, substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };
    match host
        .converge(
            &channel,
            &target,
            &rationale,
            author,
            crate::host::WriteAuth::Human,
        )
        .await
    {
        Ok(()) => {
            spawn_channel_sync(substrate.clone(), source_id);
            if let Ok(target_id) = target.parse::<ChannelId>() {
                spawn_channel_sync(substrate, target_id);
            }
            Redirect::to(&format!("/channels/{target}")).into_response()
        }
        Err(err) => lineage_error(err),
    }
}

/// The form body for starting a side-quest (`docs/adr/0027`).
#[derive(Debug, Deserialize)]
struct DivergeForm {
    child_name: String,
}

/// The form body for converging into another channel (`docs/adr/0027`).
#[derive(Debug, Deserialize)]
struct ConvergeForm {
    /// The target channel's id (the dropdown's option value).
    target: String,
    rationale: String,
}

/// The form body of a verification act: just the rationale — the act and
/// target come from the URL, the author from git config, and no member code
/// is asked of a human (the host derives the author itself and stores the
/// codes; demanding one back is friction, not safety — see
/// [`Host::authorize_human_write`]).
#[derive(Debug, Deserialize)]
struct ActForm {
    rationale: String,
    /// Where to return after acting (the focus board sets "/"); only local
    /// paths are honored.
    #[serde(default)]
    back: String,
}

/// A safe local redirect target: an absolute path on this host, nothing that
/// a browser could read as a different origin.
fn safe_back(back: &str) -> Option<&str> {
    (back.starts_with('/') && !back.starts_with("//")).then_some(back)
}

/// Append one verification act from the channel page's forms.
async fn verify(
    State(host): State<Arc<Host>>,
    Path((channel, entry, act)): Path<(String, String, String)>,
    Form(form): Form<ActForm>,
) -> Response {
    let rationale = form.rationale.trim().to_string();
    if rationale.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "a rationale is required — it's a rationale, not a checkbox",
        )
            .into_response();
    }
    let Ok(target) = entry.parse::<EntryId>() else {
        return (
            StatusCode::BAD_REQUEST,
            format!("'{entry}' is not an entry id"),
        )
            .into_response();
    };

    let resolution = match host.resolve(&channel).await {
        Ok(resolution) => resolution,
        Err(err) => return internal(format!("resolving '{channel}': {err}")),
    };
    let (substrate, ledger, id) = match resolution {
        Resolution::Resolved {
            substrate,
            ledger,
            id,
        } => (substrate, ledger, id),
        Resolution::NotFound => {
            return (StatusCode::NOT_FOUND, format!("no channel '{channel}'")).into_response();
        }
        Resolution::Ambiguous(_) => {
            return (
                StatusCode::CONFLICT,
                format!("channel name '{channel}' is ambiguous; use the id"),
            )
                .into_response();
        }
    };

    // Validate the act against the target's kind so a stale form (or a typo'd
    // URL) gets a clear refusal instead of a silently-ignored dangling act.
    let mut guard = ledger.lock().await;
    let view = match guard.project(&id).await {
        Ok(view) => view,
        Err(err) => return internal(format!("projection failed: {err}")),
    };
    let payload = match act.as_str() {
        "ratify" | "park" if view.standing(&target).is_some() => match act.as_str() {
            "ratify" => EntryPayload::Ratification {
                target,
                rationale: rationale.clone(),
            },
            _ => EntryPayload::Park {
                target,
                rationale: rationale.clone(),
            },
        },
        "approve" | "reject" if view.gate_status(&target).is_some() => match act.as_str() {
            "approve" => EntryPayload::Approval {
                target,
                rationale: rationale.clone(),
            },
            _ => EntryPayload::Rejection {
                target,
                rationale: rationale.clone(),
            },
        },
        "ratify" | "park" => {
            return (
                StatusCode::BAD_REQUEST,
                format!("{target} is not an assertion in this channel"),
            )
                .into_response();
        }
        "approve" | "reject" => {
            return (
                StatusCode::BAD_REQUEST,
                format!("{target} is not a proposal in this channel"),
            )
                .into_response();
        }
        other => {
            return (StatusCode::NOT_FOUND, format!("unknown act '{other}'")).into_response();
        }
    };

    // The author: the machine user's git identity, resolved against the
    // channel's home substrate (docs/adr/0012 — claimed, not verified).
    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return internal(format!(
                "no author identity: {err} (set git config user.name / user.email)"
            ));
        }
    };

    // The human-surface guardrail: the (host-derived) author must be in the
    // channel's Party. No member code — the host stores those itself; see
    // Host::authorize_human_write. The refusal is rare (a git identity that
    // was never granted membership), so a plain message suffices.
    if let Err(err) = host.authorize_human_write(&view, &author) {
        return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
    }

    let mut entry = LedgerEntry {
        signature: None,
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload,
    };
    host.sign_entry(&mut entry);
    if let Err(err) = guard.append(entry).await {
        return internal(format!("append failed: {err}"));
    }
    drop(guard);

    // If this approval resolved a code-PR open-PR gate, open the PR now
    // (docs/adr/0029) — best-effort, before the sync so the PR record rides it.
    if act == "approve" {
        crate::launch::execute_pr_gate_if_approved(&host, id, target).await;
    }

    // Auto-sync, best-effort and non-blocking: the human write surface is
    // terminal-less, so the page is the human's *only* affordance — without
    // this their verification sits machine-local until some agent happens to
    // run sync_channel. The redirect never waits on the network; a failed
    // sync only delays (the entry is already durable in local refs and rides
    // the next successful sync). It runs on a *fresh* substrate handle, not
    // the shared ledger: holding the ledger lock across a network push would
    // stall the very page the redirect lands on (git itself serializes
    // concurrent ref updates, so no shared lock is needed).
    let repo = substrate.clone();
    tokio::spawn(async move {
        let mut fresh = junto_substrate_git::GitRefsSubstrate::open(repo);
        if let Err(err) = fresh.sync("origin", &id).await {
            tracing::warn!("auto-sync of channel {id} after a web write failed: {err:#}");
        }
    });

    // Back where the act came from (the focus board sends "/"), defaulting
    // to the channel page, id-addressed (ids are URL-safe; names may not be).
    let destination = safe_back(&form.back)
        .map(str::to_string)
        .unwrap_or_else(|| format!("/channels/{id}"));
    Redirect::to(&destination).into_response()
}

/// The form body of a founder-only revoke/retire act: just the
/// rationale — mirrors [`ActForm`] minus `back` (these are non-HTML acts,
/// no page to return to); no member code, the host derives the author
/// and checks founder authority itself.
#[derive(Debug, Deserialize)]
struct RationaleForm {
    rationale: String,
}

/// [`retire_device`]'s and [`revoke_member`]'s shared success response:
/// how many key grants were parked.
#[derive(Serialize)]
struct ParkedDto {
    parked: usize,
}

/// Refuse an empty rationale, in the same voice [`verify`] already uses.
// Response-as-error, cold Err path — same reasoning as `project` above.
#[allow(clippy::result_large_err)]
fn require_rationale(rationale: &str) -> Result<String, Response> {
    let rationale = rationale.trim().to_string();
    if rationale.is_empty() {
        return Err(identity_error(
            StatusCode::BAD_REQUEST,
            "a rationale is required — it's a rationale, not a checkbox",
        ));
    }
    Ok(rationale)
}

/// Resolve `channel` to its home substrate, ledger and canonical id, or
/// surface the failure exactly as [`verify`] does — shared by
/// [`retire_device`] and [`revoke_member`], which both need the raw
/// [`crate::host::SharedLedger`] (to hold its guard across a
/// `project_fresh` + `append`), unlike [`project`]/[`project_fresh`],
/// which lock and release in one call.
// Response-as-error, cold Err path — same reasoning as `project` above.
#[allow(clippy::result_large_err)]
async fn resolve_for_act(
    host: &Host,
    channel: &str,
) -> Result<(std::path::PathBuf, crate::host::SharedLedger, ChannelId), Response> {
    let resolution = host.resolve(channel).await.map_err(|err| {
        identity_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("resolving '{channel}': {err}"),
        )
    })?;
    match resolution {
        Resolution::Resolved {
            substrate,
            ledger,
            id,
        } => Ok((substrate, ledger, id)),
        Resolution::NotFound => Err(identity_error(
            StatusCode::NOT_FOUND,
            format!("no channel '{channel}'"),
        )),
        Resolution::Ambiguous(_) => Err(identity_error(
            StatusCode::CONFLICT,
            format!("channel name '{channel}' is ambiguous; use the id"),
        )),
    }
}

/// `POST /channels/{channel}/keys/{grant}/retire` — retire exactly one key
/// grant, named by the entry that granted it (device-key-enrollment plan,
/// Task 10): the HTTP counterpart of `junto retire-device`
/// (`main.rs::retire_device`). Mirrors that CLI path exactly — two
/// implementations of revocation is the drift this design fights.
///
/// Founder-only (via [`crate::identity::require_founder`]), but
/// deliberately still works on a **founder's own** grant (rotation) —
/// unlike [`revoke_member`], which refuses the founder outright.
/// Refuses a retire that would leave the founder with zero active grants
/// (finding 8, final fix wave) — the same lockout `revoke_member` already
/// refuses to produce for the founder, and `keys.json` publishes exactly
/// the grant ids needed to aim one at it. Projects with
/// `guard.project_fresh` (not the cached [`project`]): a grant the
/// CLI retired seconds ago in a separate process must not be parked
/// twice off a stale fold. The ledger guard is dropped before the
/// best-effort background sync — never held across the push.
async fn retire_device(
    State(host): State<Arc<Host>>,
    Path((channel, grant)): Path<(String, String)>,
    Form(form): Form<RationaleForm>,
) -> Response {
    let rationale = match require_rationale(&form.rationale) {
        Ok(rationale) => rationale,
        Err(response) => return response,
    };
    let Ok(target) = grant.parse::<EntryId>() else {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!("'{grant}' is not an entry id"),
        );
    };
    let (substrate, ledger, id) = match resolve_for_act(&host, &channel).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let mut guard = ledger.lock().await;
    let view = match guard.project_fresh(&id).await {
        Ok(view) => view,
        Err(err) => {
            return identity_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("projection failed: {err}"),
            );
        }
    };

    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return identity_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("no author identity: {err} (set git config user.name / user.email)"),
            );
        }
    };
    if let Err(err) = crate::identity::require_founder(&view, &author, &channel) {
        return identity_error(StatusCode::FORBIDDEN, format!("{err:#}"));
    }

    match view
        .keyring
        .values()
        .flatten()
        .find(|grant| grant.granted_by == target)
    {
        None => {
            return identity_error(
                StatusCode::NOT_FOUND,
                format!(
                    "'{grant}' does not name a key-granting entry in channel '{channel}' — \
                     check the keys view for the granted_by id to pass here"
                ),
            );
        }
        Some(found) if found.retired_at.is_some() => {
            return identity_error(
                StatusCode::CONFLICT,
                format!(
                    "grant '{grant}' is already retired — parking it again would not change \
                     anything"
                ),
            );
        }
        Some(_) => {}
    }

    if crate::identity::retiring_would_strand_founder(&view, target) {
        return identity_error(
            StatusCode::BAD_REQUEST,
            "retiring this grant would leave the founder with zero active key grants — \
             enroll the replacement device first (POST /devices/enroll then /members), then \
             retire this grant once the new one is in place",
        );
    }

    let mut entry = LedgerEntry {
        signature: None,
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload: EntryPayload::Park { target, rationale },
    };
    host.sign_entry(&mut entry);
    if let Err(err) = guard.append(entry).await {
        return identity_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("append failed: {err}"),
        );
    }
    drop(guard);

    spawn_channel_sync(substrate, id);
    axum::Json(ParkedDto { parked: 1 }).into_response()
}

/// `POST /channels/{channel}/members/{email}/revoke` — park every ACTIVE
/// key grant `email` holds, in one act (device-key-enrollment plan, Task
/// 10): the HTTP counterpart of `junto revoke-member`
/// (`main.rs::revoke_member`). Refuses to revoke the channel's own
/// founder — `require_founder` only gates who may revoke, nothing gates
/// who may *be* revoked, and parking the founder's own grants would hand
/// them a cutoff and unrecognize everything they author afterward. Never
/// removes `email` from the party — recognition is party-set membership
/// (`docs/adr/0035`). Projects with `guard.project_fresh`, same reasoning
/// as [`retire_device`], and drops the guard before the background sync.
async fn revoke_member(
    State(host): State<Arc<Host>>,
    Path((channel, email)): Path<(String, String)>,
    Form(form): Form<RationaleForm>,
) -> Response {
    let rationale = match require_rationale(&form.rationale) {
        Ok(rationale) => rationale,
        Err(response) => return response,
    };
    let (substrate, ledger, id) = match resolve_for_act(&host, &channel).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let mut guard = ledger.lock().await;
    let view = match guard.project_fresh(&id).await {
        Ok(view) => view,
        Err(err) => {
            return identity_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("projection failed: {err}"),
            );
        }
    };

    let author = match crate::host::git_user(&substrate) {
        Ok(author) => author,
        Err(err) => {
            return identity_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("no author identity: {err} (set git config user.name / user.email)"),
            );
        }
    };
    if let Err(err) = crate::identity::require_founder(&view, &author, &channel) {
        return identity_error(StatusCode::FORBIDDEN, format!("{err:#}"));
    }

    if crate::identity::is_founder(&view, &email) {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!(
                "'{email}' is the founder of '{channel}' — revoke-member would park every one \
                 of the founder's own key grants and unrecognize everything they author from \
                 that moment on. To rotate one of the founder's own machines, retire just that \
                 device instead; or, when moving to a new device, enroll it FIRST and only \
                 retire the old device's grant once the new one is in place"
            ),
        );
    }

    let targets = crate::identity::grants_to_park(&view, &email);
    if targets.is_empty() {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!(
                "{email} has no active key grants in channel '{channel}' — nothing to revoke \
                 (already fully retired, or never held a key)"
            ),
        );
    }

    for target in &targets {
        let mut entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: author.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target: *target,
                rationale: rationale.clone(),
            },
        };
        host.sign_entry(&mut entry);
        if let Err(err) = guard.append(entry).await {
            return identity_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("append failed: {err}"),
            );
        }
    }
    drop(guard);

    spawn_channel_sync(substrate, id);
    axum::Json(ParkedDto {
        parked: targets.len(),
    })
    .into_response()
}

async fn channel_brief(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view, _substrate)) => {
            let name = view.name.clone().unwrap_or_else(|| channel.clone());
            let lineage = host.lineage_context(&view).await.unwrap_or_default();
            (
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                render::brief_markdown(&name, &id, &view, &lineage),
            )
                .into_response()
        }
        Err(response) => response,
    }
}

/// The list of open channels (name + id) — feeds a native surface's channel
/// picker / type-ahead. Read-only.
async fn channels_json(State(host): State<Arc<Host>>) -> Response {
    #[derive(Serialize)]
    struct Item {
        id: String,
        name: String,
    }
    let items: Vec<Item> = host
        .inventory()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| {
            s.name.map(|name| Item {
                id: s.id.to_string(),
                name,
            })
        })
        .collect();
    axum::Json(items).into_response()
}

/// The **focus board** across channels — the cross-channel "needs you" items
/// (pending gates, approved-but-unexecuted actionable gates, provisional
/// assertions awaiting verification). The native attention home renders this.
async fn focus_json(State(host): State<Arc<Host>>) -> Response {
    #[derive(Serialize)]
    struct Item {
        kind: String,
        entry_id: String,
        channel: String,
        channel_name: Option<String>,
        author: String,
        summary: String,
    }
    let clip = |s: &str| s.chars().take(140).collect::<String>();
    let inventory = host.inventory().await.unwrap_or_default();
    let mut items = Vec::new();
    for summary in &inventory {
        let Ok((id, view, _)) = project(&host, &summary.id.to_string()).await else {
            continue;
        };
        let group = crate::host::attention_for_view(&id, &view);
        for item in &group.items {
            let kind = match item.kind {
                crate::host::AttentionKind::Gate => "gate",
                crate::host::AttentionKind::AwaitingExecution => "awaiting-execution",
                crate::host::AttentionKind::Verification => "verification",
            };
            let text = match &item.entry.payload {
                EntryPayload::Assertion { statement, .. } => clip(statement),
                EntryPayload::Proposal { action, .. } => clip(action),
                _ => "—".into(),
            };
            items.push(Item {
                kind: kind.into(),
                entry_id: item.entry.id.to_string(),
                channel: id.to_string(),
                channel_name: group.name.clone(),
                author: item.entry.author.display_name.clone(),
                summary: text,
            });
        }
    }
    axum::Json(items).into_response()
}

/// Machine settings for the native settings view: harness status, the
/// harnesses available to the agent form, registered substrates, the identity
/// human acts author as, and the host version. Read-only.
async fn settings_json(State(host): State<Arc<Host>>) -> Response {
    #[derive(Serialize)]
    struct HarnessOut {
        protocol: &'static str,
        detail: String,
        backend: &'static str,
        auth: &'static str,
        hint: Option<&'static str>,
    }
    #[derive(Serialize)]
    struct HarnessRef {
        id: &'static str,
        label: &'static str,
    }
    #[derive(Serialize)]
    struct IdentityOut {
        name: String,
        email: String,
    }
    #[derive(Serialize)]
    struct Out {
        harness: HarnessOut,
        harnesses: Vec<HarnessRef>,
        substrates: Vec<String>,
        identity: Option<IdentityOut>,
        version: &'static str,
    }
    let status = crate::launch::harness_status();
    let substrates = host.substrate_paths().unwrap_or_default();
    let identity = substrates
        .first()
        .and_then(|repo| crate::host::git_user(repo).ok())
        .map(|m| IdentityOut {
            name: m.display_name,
            email: m.email,
        });
    let out = Out {
        harness: HarnessOut {
            protocol: status.protocol,
            detail: status.detail,
            backend: status.backend,
            auth: status.auth,
            hint: status.hint,
        },
        harnesses: crate::launch::all_harnesses()
            .iter()
            .map(|h| HarnessRef {
                id: h.id,
                label: h.label,
            })
            .collect(),
        substrates: substrates.iter().map(|p| p.display().to_string()).collect(),
        identity,
        version: env!("CARGO_PKG_VERSION"),
    };
    axum::Json(out).into_response()
}

/// The registered **home substrates** (repo paths) — for the native new-channel
/// form to pick which substrate a channel opens in (when more than one).
async fn substrates_json(State(host): State<Arc<Host>>) -> Response {
    let paths: Vec<String> = host
        .substrate_paths()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.display().to_string())
        .collect();
    axum::Json(paths).into_response()
}

/// Distinct **workspace repos**, most-recently-used first — the launch form's
/// inferred default + suggestions for a fresh channel (so the user rarely has
/// to pick a directory). Ordered by the last activity of the channel each repo
/// is bound to.
async fn workspaces_json(State(host): State<Arc<Host>>) -> Response {
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    let mappings = match crate::launch::all_workspaces(&junto_home) {
        Ok(mappings) => mappings,
        Err(err) => return internal(format!("reading workspaces: {err}")),
    };
    // Channel id → last activity, for recency ordering.
    let activity: std::collections::HashMap<String, i64> = host
        .inventory()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            (
                s.id.to_string(),
                s.last_activity.map(|t| t.as_millis()).unwrap_or(0),
            )
        })
        .collect();
    // Newest channel's repo first; dedup repos keeping their best (latest) rank.
    let mut ranked: Vec<(i64, String)> = mappings
        .into_iter()
        .map(|(channel, repo)| {
            let when = activity.get(&channel.to_string()).copied().unwrap_or(0);
            (when, repo.display().to_string())
        })
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut seen = std::collections::HashSet::new();
    let repos: Vec<String> = ranked
        .into_iter()
        .filter_map(|(_, repo)| seen.insert(repo.clone()).then_some(repo))
        .collect();
    axum::Json(repos).into_response()
}

/// The configured **Agents** the launch picker offers (`docs/adr/0023`/`0024`).
/// The first entry is the default (the stock agent for the default harness).
/// The native launch controls render this as the agent dropdown.
async fn agents_json() -> Response {
    #[derive(Serialize)]
    struct McpServerItem {
        name: String,
        url: String,
    }
    #[derive(Serialize)]
    struct Item {
        slug: String,
        name: String,
        harness: String,
        model: Option<String>,
        role: Option<String>,
        mcp_servers: Vec<McpServerItem>,
        skills: Vec<String>,
        plugins: Vec<String>,
    }
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    match crate::agent::all_agents(&junto_home) {
        Ok(agents) => {
            let items: Vec<Item> = agents
                .into_iter()
                .map(|a| Item {
                    slug: a.slug,
                    name: a.name,
                    harness: a.harness,
                    model: a.model,
                    role: a.role,
                    mcp_servers: a
                        .mcp_servers
                        .into_iter()
                        .map(|s| McpServerItem {
                            name: s.name,
                            url: s.url,
                        })
                        .collect(),
                    skills: a.skills,
                    plugins: a.plugins,
                })
                .collect();
            axum::Json(items).into_response()
        }
        Err(err) => internal(format!("reading agents: {err}")),
    }
}

/// The whole **lineage DAG** across channels (nodes + diverge/converge edges) —
/// the data a native surface draws as a git-style branch graph. Read-only.
async fn lineage_json(State(host): State<Arc<Host>>) -> Response {
    #[derive(Serialize)]
    struct Milestone {
        ms: i64,
        label: String,
    }
    #[derive(Serialize)]
    struct Node {
        id: String,
        name: String,
        first_ms: Option<i64>,
        last_ms: Option<i64>,
        /// Key points along the channel's track, each with explanatory text.
        milestones: Vec<Milestone>,
    }
    #[derive(Serialize)]
    struct Edge {
        from: String,
        to: String,
        relation: String,
    }
    #[derive(Serialize)]
    struct Graph {
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    }

    let clip = |s: &str| s.chars().take(70).collect::<String>();
    let inventory = host.inventory().await.unwrap_or_default();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for summary in &inventory {
        let Some(name) = &summary.name else { continue };
        let mut milestones = Vec::new();
        if let Ok((id, view, _)) = project(&host, &summary.id.to_string()).await {
            // Use each channel's *outgoing* edges so each edge is counted once.
            for edge in &view.lineage {
                if edge.direction == junto_kernel::LineageDirection::Outgoing {
                    edges.push(Edge {
                        from: id.to_string(),
                        to: edge.other.to_string(),
                        relation: format!("{:?}", edge.relation).to_lowercase(),
                    });
                }
            }
            // Milestones: the subject points along the track, each labelled.
            for entry in &view.entries {
                let label = match &entry.payload {
                    EntryPayload::Assertion { statement, .. } => {
                        Some(format!("decision · {}", clip(statement)))
                    }
                    EntryPayload::Proposal { action, .. } => {
                        Some(format!("gate · {}", clip(action)))
                    }
                    EntryPayload::SessionStarted { intent } => {
                        Some(format!("session · {}", clip(intent)))
                    }
                    EntryPayload::DivergedFrom { .. } => Some("diverged from parent".into()),
                    EntryPayload::ConvergedInto { .. } => Some("converged into another".into()),
                    _ => None,
                };
                if let Some(label) = label {
                    milestones.push(Milestone {
                        ms: entry.timestamp.as_millis(),
                        label,
                    });
                }
            }
            // Keep the most recent dozen so the track doesn't get crowded.
            if milestones.len() > 12 {
                milestones.drain(0..milestones.len() - 12);
            }
        }
        nodes.push(Node {
            id: summary.id.to_string(),
            name: name.clone(),
            first_ms: summary.first_activity.map(|t| t.as_millis()),
            last_ms: summary.last_activity.map(|t| t.as_millis()),
            milestones,
        });
    }
    axum::Json(Graph { nodes, edges }).into_response()
}

/// A structured **read-only** projection of a channel as JSON — the data a
/// non-HTML surface (e.g. the native Iced spike, `docs/native-ui-toolkit-
/// assessment.md`) renders into its own widgets. Mirrors what the HTML channel
/// page shows: party, lineage edges (the split history), and the entry timeline
/// with each entry's derived status. Read-only; acts stay POST routes.
async fn channel_view_json(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view, _substrate)) => {
            // Resolve lineage edges' other-channel ids to names for the strip.
            let names: std::collections::HashMap<String, String> = host
                .inventory()
                .await
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.name.map(|n| (s.id.to_string(), n)))
                .collect();
            let mut dto = ChannelDto::from_view(&id, &view, &names);
            // The channel's remembered workspace, if any — lets the native launch
            // form pre-fill it instead of asking the user to pick a directory.
            dto.workspace = crate::host::junto_home()
                .ok()
                .and_then(|home| crate::launch::workspace_for(&home, &id).ok().flatten())
                .map(|repo| repo.display().to_string());
            axum::Json(dto).into_response()
        }
        Err(response) => response,
    }
}

#[derive(Serialize)]
struct ChannelDto {
    id: String,
    name: Option<String>,
    closed: bool,
    party: Vec<String>,
    /// The channel's remembered workspace repo, if any (a returning channel).
    workspace: Option<String>,
    lineage: Vec<LineageDto>,
    sessions: Vec<SessionDto>,
    entries: Vec<EntryDto>,
}

#[derive(Serialize)]
struct SessionDto {
    id: String,
    state: String,
    intent: String,
}

#[derive(Serialize)]
struct LineageDto {
    relation: String,
    direction: String,
    other: String,
    other_name: Option<String>,
    label: String,
}

#[derive(Serialize)]
struct EntryDto {
    id: String,
    author: String,
    kind: String,
    summary: String,
    status: Option<String>,
    unrecognized: bool,
    /// Recognized but the signature is missing or does not verify against
    /// the author's recorded key that was active at the entry's own
    /// timestamp (`docs/adr/0033`, `ChannelView::unverified`).
    unverified: bool,
    /// The entry this one acts on, if any — e.g. a SessionUpdated/ArtifactAttached
    /// points at its SessionStarted, letting a surface group a session's record.
    target: Option<String>,
    /// The decision frame's articulated options (`docs/adr/0019`), if any — the
    /// pre-baked, one-click rationales a verifier can adopt. Empty when none.
    frame: Vec<FrameOptionDto>,
}

/// One decision-frame option for the native surface: a labelled choice that
/// performs `act` with a drafted `rationale`.
#[derive(Serialize)]
struct FrameOptionDto {
    label: String,
    act: String,
    rationale: String,
}

/// The act-route segment for a [`junto_kernel::FrameAct`].
fn frame_act_route(act: junto_kernel::FrameAct) -> &'static str {
    match act {
        junto_kernel::FrameAct::Ratify => "ratify",
        junto_kernel::FrameAct::Park => "park",
        junto_kernel::FrameAct::Approve => "approve",
        junto_kernel::FrameAct::Reject => "reject",
    }
}

impl ChannelDto {
    fn from_view(
        id: &ChannelId,
        view: &ChannelView,
        names: &std::collections::HashMap<String, String>,
    ) -> Self {
        let entries = view
            .entries
            .iter()
            .map(|entry| EntryDto::from_entry(entry, view))
            .collect();
        let lineage = view
            .lineage
            .iter()
            .map(|edge| LineageDto {
                relation: format!("{:?}", edge.relation).to_lowercase(),
                direction: format!("{:?}", edge.direction).to_lowercase(),
                other: edge.other.to_string(),
                other_name: names.get(&edge.other.to_string()).cloned(),
                label: lineage_label(edge),
            })
            .collect();
        // Sessions: each SessionStarted entry + its folded state.
        let sessions = view
            .entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                EntryPayload::SessionStarted { intent } => Some(SessionDto {
                    id: entry.id.to_string(),
                    state: view
                        .sessions
                        .get(&entry.id)
                        .map(|s| format!("{:?}", s.state).to_lowercase())
                        .unwrap_or_else(|| "unknown".into()),
                    intent: intent.clone(),
                }),
                _ => None,
            })
            .collect();
        ChannelDto {
            id: id.to_string(),
            name: view.name.clone(),
            closed: view.closed,
            party: view.party.iter().map(|m| m.display_name.clone()).collect(),
            workspace: None, // set by the handler (needs junto_home)
            lineage,
            sessions,
            entries,
        }
    }
}

impl EntryDto {
    fn from_entry(entry: &LedgerEntry, view: &ChannelView) -> Self {
        let (kind, summary) = match &entry.payload {
            EntryPayload::ChannelOpened { .. } => ("channel", "opened the channel".to_string()),
            EntryPayload::MemberAdded { member } => {
                ("member", format!("added {}", member.display_name))
            }
            EntryPayload::ChannelClosed { rationale } => {
                ("channel", format!("closed — {rationale}"))
            }
            EntryPayload::ChannelReopened { rationale } => {
                ("channel", format!("reopened — {rationale}"))
            }
            EntryPayload::DivergedFrom { .. } => ("lineage", "diverged from a parent".to_string()),
            EntryPayload::ChildDiverged { .. } => {
                ("lineage", "a side-quest diverged from here".to_string())
            }
            EntryPayload::ConvergedInto { .. } => {
                ("lineage", "converged into another channel".to_string())
            }
            EntryPayload::ConvergenceReceived { .. } => {
                ("lineage", "received a convergence".to_string())
            }
            EntryPayload::Assertion { statement, .. } => ("assertion", statement.clone()),
            EntryPayload::Ratification { rationale, .. } => {
                ("act", format!("ratified — {rationale}"))
            }
            EntryPayload::Park { rationale, .. } => ("act", format!("parked — {rationale}")),
            EntryPayload::Correction { statement, .. } => {
                ("act", format!("corrected — {statement}"))
            }
            EntryPayload::Proposal { action, .. } => ("proposal", action.clone()),
            EntryPayload::Approval { rationale, .. } => ("act", format!("approved — {rationale}")),
            EntryPayload::Rejection { rationale, .. } => ("act", format!("rejected — {rationale}")),
            EntryPayload::GateExecuted { note, .. } => ("act", format!("gate executed — {note}")),
            EntryPayload::SessionStarted { intent } => ("session", intent.clone()),
            EntryPayload::SessionUpdated { note, .. } => ("session", note.clone()),
            EntryPayload::ArtifactAttached {
                kind, description, ..
            } => ("artifact", format!("{kind}: {description}")),
        };
        let status = match &entry.payload {
            EntryPayload::Assertion { .. } | EntryPayload::Correction { .. } => view
                .standings
                .get(&entry.id)
                .map(|s| format!("{s:?}").to_lowercase()),
            EntryPayload::Proposal { .. } => view
                .gate_status
                .get(&entry.id)
                .map(|s| format!("{s:?}").to_lowercase()),
            EntryPayload::SessionStarted { .. } => view
                .sessions
                .get(&entry.id)
                .map(|s| format!("{:?}", s.state).to_lowercase()),
            _ => None,
        };
        let frame = match &entry.payload {
            EntryPayload::Assertion { frame, .. } | EntryPayload::Proposal { frame, .. } => frame
                .as_ref()
                .map(|f| {
                    f.options
                        .iter()
                        .map(|o| FrameOptionDto {
                            label: o.label.clone(),
                            act: frame_act_route(o.act).to_string(),
                            rationale: o.rationale.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        EntryDto {
            id: entry.id.to_string(),
            author: entry.author.display_name.clone(),
            kind: kind.to_string(),
            summary,
            status,
            unrecognized: view.unrecognized.contains(&entry.id),
            unverified: view.unverified.contains(&entry.id),
            target: entry.payload.target().map(|t| t.to_string()),
            frame,
        }
    }
}

/// A human label for a lineage edge from this channel's point of view.
fn lineage_label(edge: &junto_kernel::LineageEdge) -> String {
    use junto_kernel::{LineageDirection as D, LineageRelation as R};
    match (edge.relation, edge.direction) {
        (R::Diverge, D::Incoming) => "diverged from parent".to_string(),
        (R::Diverge, D::Outgoing) => "side-quest diverged from here".to_string(),
        (R::Converge, D::Incoming) => "received a convergence".to_string(),
        (R::Converge, D::Outgoing) => "converged into another channel".to_string(),
    }
}

/// A channel's key grants, as fingerprints — the host's read surface for
/// "whose devices are these" (device-key-enrollment plan, Task 6). Today
/// only the CLI's `keys list` and the live-WebSocket handshake read
/// `ChannelView::keyring`; this is what lets a non-terminal surface (a
/// native GUI) show a member's devices at all. Uses [`project_fresh`], not
/// [`project`]: a `revoke-member`/`retire-device` run in a separate `junto`
/// process must be visible to the very next refresh, not hidden behind this
/// process's cached fold. Read-only — revocation stays a CLI/MCP act.
async fn keys_json(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project_fresh(&host, &channel).await {
        Ok((_id, view, substrate)) => {
            // Best-effort: a host with no git config still answers with a
            // roster, just no identity of its own (the brief's rule).
            let viewer = crate::host::git_user(&substrate).ok();
            let viewer_is_founder = viewer
                .as_ref()
                .is_some_and(|member| crate::identity::is_founder(&view, &member.email));
            let dto = KeysDto {
                founder_email: view
                    .party
                    .first()
                    .map(|f| f.email.clone())
                    .unwrap_or_default(),
                viewer_email: viewer.map(|member| member.email),
                viewer_is_founder,
                members: view
                    .party
                    .iter()
                    .map(|member| KeyMemberDto::from_member(member, &view))
                    .collect(),
            };
            axum::Json(dto).into_response()
        }
        Err(response) => as_identity_error(response).await,
    }
}

#[derive(Serialize)]
struct KeysDto {
    founder_email: String,
    /// The git identity this host writes as — the surface needs it to know
    /// whose devices these are. `None` when `git_user` fails (no git
    /// config); the endpoint still answers 200, since reading a roster
    /// needs no identity of its own.
    viewer_email: Option<String>,
    /// Whether that identity may perform the founder-only acts, so the GUI
    /// shows or hides them instead of guessing. `false` whenever
    /// `viewer_email` is `None`.
    viewer_is_founder: bool,
    /// `view.party` order — founder first, one row per email, same
    /// ordering the party projection guarantees everywhere else.
    members: Vec<KeyMemberDto>,
}

#[derive(Serialize)]
struct KeyMemberDto {
    display_name: String,
    email: String,
    /// "human" | "agent" — lowercase, like `EntryDto`'s `kind`/`status`.
    kind: String,
    /// Keyring order (canonical entry order) — stable across replicas.
    devices: Vec<KeyGrantDto>,
    /// Every grant retired: this email is revoked as of the latest
    /// retirement (`docs/adr/0035`'s all-retired rule — see
    /// [`crate::identity::is_revoked`], the one place it is written down).
    /// `false` for a member with no grants at all.
    revoked: bool,
}

impl KeyMemberDto {
    fn from_member(member: &junto_kernel::Member, view: &ChannelView) -> Self {
        let grants = view.keyring.get(&member.email);
        let revoked = crate::identity::is_revoked(view, &member.email);
        KeyMemberDto {
            display_name: member.display_name.clone(),
            email: member.email.clone(),
            kind: match member.kind {
                junto_kernel::MemberKind::Human => "human",
                junto_kernel::MemberKind::Agent => "agent",
            }
            .to_string(),
            devices: grants
                .map(|grants| grants.iter().map(KeyGrantDto::from_grant).collect())
                .unwrap_or_default(),
            revoked,
        }
    }
}

#[derive(Serialize)]
struct KeyGrantDto {
    /// 16 hex chars of the signing key ([`crate::identity::fingerprint`]).
    /// Never the whole public key.
    fingerprint: String,
    /// 16 hex chars of the device's transport key (`docs/adr/0033` two-key
    /// separation). `None` for a grant made before transport keys existed,
    /// or a keyless member's grant.
    transport_fingerprint: Option<String>,
    /// The entry id that authorized this key — `retire-device`'s `--grant`
    /// handle.
    granted_by: String,
    /// Epoch millis, when retired.
    retired_at: Option<i64>,
}

impl KeyGrantDto {
    fn from_grant(grant: &junto_kernel::KeyGrant) -> Self {
        KeyGrantDto {
            fingerprint: crate::identity::fingerprint(&grant.key),
            transport_fingerprint: grant
                .transport_key
                .as_ref()
                .map(crate::identity::fingerprint),
            granted_by: grant.granted_by.to_string(),
            retired_at: grant.retired_at.map(|ts| ts.as_millis()),
        }
    }
}

/// The form body for minting a multi-channel enrollment invite: `channel`
/// may repeat, one value per channel the invite should cover. Read as raw
/// pairs ([`InviteForm::from_pairs`]), the same technique `save_agent`
/// already uses for its `skill`/`plugin_path` rows — axum's typed `Form`
/// can't collect duplicate keys into a `Vec`.
struct InviteForm {
    member: String,
    channel: Vec<String>,
}

impl InviteForm {
    fn from_pairs(pairs: &[(String, String)]) -> Self {
        InviteForm {
            member: field(pairs, "member").to_string(),
            channel: all_fields(pairs, "channel"),
        }
    }
}

/// Mint a founder-issued, multi-channel enrollment invite over HTTP
/// (device-key-enrollment plan) — the human-surface counterpart of `junto
/// invite` (`main.rs::mint_invite`). The authority and validation rules
/// mirror that CLI path closely, on purpose: both surfaces mint the same
/// kind of bearer grant, and drift between them is exactly how a code the
/// CLI would refuse could get minted here (or vice versa) — see the
/// `member` bounds below, added after a review caught this endpoint
/// skipping them.
///
/// `invites::prune` runs first (this is the human surface's most likely
/// "actively enrolling" moment, mirroring why the CLI prunes here too).
/// `member` is then checked non-empty (after trimming) and within
/// `enroll::MAX_FIELD_CHARS` — a blank member would mint a real,
/// redeemable record for the empty string (`invites::consume` compares
/// the payload's email to the record's, and `""` matches `""`), and an
/// oversized one would mint a code `enroll::decode_invite`'s
/// `check_field_bounds` could only ever reject later, burning the
/// resolution work and the token for nothing (`main.rs::mint_invite`
/// checks the same bound, at `main.rs:880-885`). Every named channel is
/// then resolved and re-projected with [`project_fresh`] (not the cached
/// [`project`] — a `revoke-member` in a separate process must be visible
/// to the very next mint) and checked with
/// [`crate::identity::require_founder`] — **before** a token is minted or
/// any record is issued. An invite the caller cannot complete for even
/// one named channel is refused whole, nothing left behind (400 for a
/// malformed/empty channel set or member, 403 naming the offending
/// channel for an authority failure). Two spellings of the same channel
/// (a name and its id) collapse to one record, deduped on the canonical
/// id, first-seen order.
async fn mint_invite(
    State(host): State<Arc<Host>>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let form = InviteForm::from_pairs(&pairs);
    let member = form.member.trim().to_string();

    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    if let Err(err) = crate::invites::prune(&junto_home) {
        return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }

    if member.is_empty() {
        return identity_error(StatusCode::BAD_REQUEST, "an invite must name a member");
    }
    if member.chars().count() > crate::enroll::MAX_FIELD_CHARS {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!(
                "member exceeds the {}-char limit",
                crate::enroll::MAX_FIELD_CHARS
            ),
        );
    }

    if form.channel.is_empty() {
        return identity_error(
            StatusCode::BAD_REQUEST,
            "an invite must name at least one channel",
        );
    }
    if form.channel.len() > crate::enroll::MAX_INVITE_CHANNELS {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!(
                "an invite may name at most {} channels",
                crate::enroll::MAX_INVITE_CHANNELS
            ),
        );
    }

    // Resolve every channel and prove founder authority on every one
    // BEFORE minting a token or issuing any record — the all-or-nothing
    // rule (see the doc comment above).
    let mut canonical_channels: Vec<String> = Vec::new();
    for channel in &form.channel {
        let (id, view, substrate) = match project_fresh(&host, channel).await {
            Ok(projected) => projected,
            Err(response) => return as_identity_error(response).await,
        };
        let caller = match crate::host::git_user(&substrate) {
            Ok(caller) => caller,
            Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
        if let Err(err) = crate::identity::require_founder(&view, &caller, channel) {
            return identity_error(StatusCode::FORBIDDEN, err.to_string());
        }
        let canonical = id.to_string();
        if !canonical_channels.contains(&canonical) {
            canonical_channels.push(canonical);
        }
    }

    let token = crate::enroll::mint_invite_token();
    let expires_at = Timestamp::now().as_millis() + crate::enroll::MAX_INVITE_TTL_MS;
    for canonical in &canonical_channels {
        if let Err(err) = crate::invites::issue(&junto_home, &token, &member, canonical, expires_at)
        {
            return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    }

    let payload = crate::enroll::InvitePayload {
        v: crate::enroll::PAYLOAD_VERSION,
        invite_token: token,
        member_email: member,
        channels: canonical_channels.clone(),
        expires_at,
    };
    let url = match crate::enroll::encode_invite(&payload) {
        Ok(url) => url,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    axum::Json(InviteMintedDto {
        url,
        expires_at,
        channels: canonical_channels,
    })
    .into_response()
}

/// `POST /invites`' response: the shareable `junto://invite?code=…` URI,
/// its expiry, and the resolved canonical channel ids it covers — so the
/// caller can show what it actually granted, not just what it typed.
#[derive(Serialize)]
struct InviteMintedDto {
    url: String,
    expires_at: i64,
    channels: Vec<String>,
}

/// The form body for enrolling this machine's device key from an invite.
/// Deliberately has no `email` field — see [`enroll_device`]'s doc comment
/// for why one must never be added.
#[derive(Debug, Deserialize)]
struct EnrollForm {
    invite: String,
    name: Option<String>,
}

/// Mint this device's own signing and transport keypairs from a founder's
/// invite (device-key-enrollment plan) — the human-surface counterpart of
/// `junto enroll` (`main.rs::enroll`).
///
/// This is the **only** endpoint in this codebase that may cause a mint of
/// fresh secret material (a device's Ed25519 signing and transport private
/// keys, `docs/adr/0033`) — every other write here operates on identity
/// already minted elsewhere. It must therefore **never** be reachable from
/// the mobile/remote read-only role (`docs/adr/0012`'s localhost-only write
/// surface): a network peer must never be able to trigger a local key mint
/// on this machine.
///
/// The enrolled email comes **only** from the decoded invite, never from
/// the form — there is deliberately no `email` field on [`EnrollForm`].
/// The invite token is itself the authorization (`invites::consume`'s
/// `WrongMember` check downstream verifies it), so accepting a caller-
/// supplied email would let anyone holding a valid invite for one address
/// mint a keypair under a *different* one merely by typing it in. `name`
/// supplies only the display name, defaulting to this machine's git
/// identity and, failing that, the invite email's local part. No founder
/// check and no member code: the invite token is the authorization, and
/// the caller is this machine's own localhost (`docs/adr/0012`).
async fn enroll_device(Form(form): Form<EnrollForm>) -> Response {
    let invite = match crate::enroll::decode_invite(&form.invite) {
        Ok(invite) => invite,
        Err(err) => {
            // Distinguish expiry from every other malformed shape — an
            // expired invite is routine (ask the founder for a fresh one),
            // everything else is a bug in whatever produced the code.
            let message = if err.to_string().contains("expired") {
                "this invite has expired; ask the founder to mint a fresh one".to_string()
            } else {
                format!("invalid invite: {err}")
            };
            return identity_error(StatusCode::BAD_REQUEST, message);
        }
    };

    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    let display_name = form
        .name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            crate::host::git_user(std::path::Path::new("."))
                .ok()
                .map(|member| member.display_name)
        })
        .unwrap_or_else(|| {
            invite
                .member_email
                .split('@')
                .next()
                .unwrap_or(&invite.member_email)
                .to_string()
        });
    // Finding 9c (final fix wave): bounded before it reaches the
    // payload — an unbounded name would mint a code
    // `enroll::decode_enroll`'s own `check_field_bounds` could only ever
    // reject later, on the founder's machine (the same bound
    // `mint_invite`'s `member` already enforces).
    if display_name.chars().count() > crate::enroll::MAX_FIELD_CHARS {
        return identity_error(
            StatusCode::BAD_REQUEST,
            format!(
                "name exceeds the {}-char limit",
                crate::enroll::MAX_FIELD_CHARS
            ),
        );
    }

    // No secret, no seed, and no full public key may reach the response,
    // a log line, or an error message from here down — only fingerprints.
    // `enrolled_signing_key`, never `signing_key` (finding 9a, final fix
    // wave): this mints for whatever email the decoded invite names, not
    // a locally resolved identity, so it must record `authored: false` —
    // see that function's doc comment.
    let key = match crate::keys::enrolled_signing_key(&junto_home, &invite.member_email) {
        Ok(key) => key,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    let transport_key = match crate::keys::transport_key(&junto_home, &invite.member_email) {
        Ok(key) => key,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    let payload = crate::enroll::EnrollPayload {
        v: crate::enroll::PAYLOAD_VERSION,
        invite_token: invite.invite_token,
        email: invite.member_email.clone(),
        display_name,
        public_key: key.public_key(),
        transport_public_key: transport_key.public_key(),
        expires_at: invite.expires_at,
    };
    let url = match crate::enroll::encode_enroll(&payload) {
        Ok(url) => url,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    axum::Json(EnrolledDto {
        url,
        email: invite.member_email,
        fingerprint: crate::identity::fingerprint(&key.public_key()),
        transport_fingerprint: crate::identity::fingerprint(&transport_key.public_key()),
    })
    .into_response()
}

/// `POST /devices/enroll`'s response: the shareable `junto://enroll?
/// code=…` URI plus both fingerprints — never the whole public keys, never
/// a secret. Unlike [`KeyGrantDto::transport_fingerprint`] (`Option`, for a
/// grant made before transport keys existed), `transport_fingerprint` here
/// is **never** optional: a device enrolling under the current payload
/// version always mints both keypairs in the same step.
#[derive(Serialize)]
struct EnrolledDto {
    url: String,
    email: String,
    fingerprint: String,
    transport_fingerprint: String,
}

/// Decode `code` as a `junto://enroll?code=…` URI, or a 400 response
/// distinguishing an expired code from every other malformed shape — the
/// same distinction [`enroll_device`] draws. JSON (finding 1, final fix
/// wave): shared only by [`preview_enrollment`] and
/// [`redeem_enrollment_endpoint`], both identity endpoints.
// Response-as-error, cold Err path — same reasoning as `project` above.
#[allow(clippy::result_large_err)]
fn decode_enroll_or_400(code: &str) -> Result<crate::enroll::EnrollPayload, Response> {
    crate::enroll::decode_enroll(code).map_err(|err| {
        let message = if err.to_string().contains("expired") {
            "this invite has expired; ask the founder to mint a fresh one".to_string()
        } else {
            format!("invalid enroll code: {err}")
        };
        identity_error(StatusCode::BAD_REQUEST, message)
    })
}

/// The form body of `POST /members`: a redeemed device's enroll code plus
/// the kind of member it should become (device-key-enrollment plan, Task
/// 9). `kind` is deliberately a plain `String`, not `MemberKind` itself —
/// ADR 0035 forbids defaulting *the member kind*, so both a missing and
/// an unrecognized value are refused by the handler with the identical
/// 400 (`#[serde(default)]` only lets an absent field reach that check
/// instead of axum's own 422 form-rejection — the value itself is never
/// defaulted to `"human"` or anything else).
#[derive(Debug, Deserialize)]
struct RedeemForm {
    enroll: String,
    #[serde(default)]
    kind: String,
}

/// `POST /members`'s response: one outcome per channel the invite covered
/// (device-key-enrollment plan, Task 9) — one invite may cover several
/// channels (Task 3), so this is never collapsed to a single pass/fail.
#[derive(Serialize)]
struct RedeemedDto {
    outcomes: Vec<RedeemOutcomeDto>,
}

/// One channel's outcome, in [`RedeemOutcome`]'s own vocabulary: `result`
/// is the variant in snake_case (`granted`, `already_a_member`,
/// `invite_already_used`, `not_founder`, `failed`); `detail` carries
/// `Failed`'s message (`None` for every other variant). `channel` is
/// always the canonical id; `channel_name` is resolved for display.
/// `warning` carries `Granted`'s revocation-cutoff warning (finding 5,
/// final fix wave) — `None` on every non-`Granted` outcome and on an
/// ordinary grant; this is the HTTP path's ONLY way to see it, since it
/// has no host log to read a CLI `println!` from.
#[derive(Serialize)]
struct RedeemOutcomeDto {
    channel: String,
    channel_name: Option<String>,
    result: String,
    detail: Option<String>,
    warning: Option<String>,
}

impl RedeemOutcomeDto {
    fn from_outcome(
        channel: String,
        channel_name: Option<String>,
        outcome: &RedeemOutcome,
    ) -> Self {
        let (result, detail, warning): (&str, Option<String>, Option<String>) = match outcome {
            RedeemOutcome::Granted { warning } => ("granted", None, warning.clone()),
            RedeemOutcome::AlreadyAMember => ("already_a_member", None, None),
            RedeemOutcome::InviteAlreadyUsed => ("invite_already_used", None, None),
            RedeemOutcome::NotFounder => ("not_founder", None, None),
            RedeemOutcome::Failed(reason) => ("failed", Some(reason.clone()), None),
        };
        RedeemOutcomeDto {
            channel,
            channel_name,
            result: result.to_string(),
            detail,
            warning,
        }
    }
}

/// `POST /members` — redeem an enrollment across every channel its invite
/// still covers (device-key-enrollment plan, Task 9): the HTTP
/// counterpart of `junto add-member --enroll`, a thin shell over
/// [`crate::redeem_enrollment`] (Task 4's engine — decode, parse `kind`,
/// call, serialize; never reimplemented or forked here). `granted_by` is
/// always `None`: the CLI's `--author-name`/`--author-email` override has
/// no HTTP equivalent, so every redeemed channel grants as this
/// machine's git identity in its own home substrate, exactly the engine's
/// keyless-path default.
///
/// `kind` is parsed strictly before the engine ever runs — `"human"` or
/// `"agent"`, nothing else, never defaulted (ADR 0035) — so a bad kind
/// burns no invite record. 200 when at least one channel was granted; 409
/// when none were (the whole set was already used or refused) — either
/// way the body carries every channel's own outcome, never collapsed to
/// one pass/fail. An invite whose token covers no channel at all (the
/// engine's own empty-set refusal) is reported the same way: 409, with
/// [`crate::InviteExhausted`]'s shared wording as the body.
async fn redeem_enrollment_endpoint(
    State(host): State<Arc<Host>>,
    Form(form): Form<RedeemForm>,
) -> Response {
    let payload = match decode_enroll_or_400(&form.enroll) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let kind = match form.kind.as_str() {
        "human" => junto_kernel::MemberKind::Human,
        "agent" => junto_kernel::MemberKind::Agent,
        "" => {
            return identity_error(
                StatusCode::BAD_REQUEST,
                "kind is required and must be 'human' or 'agent' — it is never defaulted \
                 (docs/adr/0035)",
            );
        }
        other => {
            return identity_error(
                StatusCode::BAD_REQUEST,
                format!("kind must be 'human' or 'agent', not '{other}'"),
            );
        }
    };

    let outcomes = match crate::redeem_enrollment(&host, &payload, kind, None).await {
        Ok(outcomes) => outcomes,
        Err(err) => {
            // The engine's only `bail!` is the exhausted-invite refusal
            // (`channels_for` returned nothing to redeem) — every other
            // `Err` is a genuine failure (e.g. an unreadable invite store),
            // which `preview_enrollment` already reports as `internal`,
            // not 409. Downcast against the typed sentinel
            // (`crate::InviteExhausted`), not a string comparison: a
            // `.context(..)` added to the `?` sites above the engine's
            // `bail!`, or a second `bail!` reusing the same wording
            // deeper in the engine, would silently reclassify a string
            // match but cannot fool a downcast.
            if err.downcast_ref::<crate::InviteExhausted>().is_some() {
                return identity_error(StatusCode::CONFLICT, err.to_string());
            }
            return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };

    let mut granted_any = false;
    let mut dtos = Vec::with_capacity(outcomes.len());
    for (channel, outcome) in &outcomes {
        if matches!(outcome, RedeemOutcome::Granted { .. }) {
            granted_any = true;
        }
        let channel_name = project(&host, channel)
            .await
            .ok()
            .and_then(|(_, view, _)| view.name);
        dtos.push(RedeemOutcomeDto::from_outcome(
            channel.clone(),
            channel_name,
            outcome,
        ));
    }

    let status = if granted_any {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    (status, axum::Json(RedeemedDto { outcomes: dtos })).into_response()
}

/// The form body of `POST /devices/preview`: just the enroll code — no
/// `kind`, since this endpoint mints and appends nothing.
#[derive(Debug, Deserialize)]
struct PreviewForm {
    enroll: String,
}

/// `POST /devices/preview`'s response: what an enroll code would grant if
/// redeemed right now (device-key-enrollment plan, Task 9).
/// `transport_fingerprint` (finding 2, final fix wave) is the founder's
/// ONLY integrity check on the transport half: the enroll code is
/// unsigned and travels by paste, so without this an altered code that
/// keeps the signing key but substitutes the transport key would pass
/// the founder's out-of-band fingerprint comparison unchanged.
#[derive(Serialize)]
struct EnrollPreviewDto {
    email: String,
    display_name: String,
    fingerprint: String,
    transport_fingerprint: String,
    channels: Vec<PreviewChannelDto>,
}

#[derive(Serialize)]
struct PreviewChannelDto {
    id: String,
    name: Option<String>,
}

/// `POST /devices/preview` — the read-only look at what an enroll code
/// would grant (device-key-enrollment plan, Task 9): decodes the payload
/// and answers from [`crate::invites::channels_for`] plus the payload's
/// own email/display-name/fingerprint. **Appends nothing and consumes
/// nothing** — never calls [`crate::redeem_enrollment`] or
/// `invites::consume`. This exists because the founder must see what they
/// are about to grant *before* anything is appended, and the channel set
/// deliberately never travels inside the code itself.
///
/// Reads `channels_for` with the payload's OWN email (finding 9b, final
/// fix wave) — the same identity comparison [`crate::invites::consume`]
/// already makes, so a token whose records were issued for a different
/// member can never enumerate that member's channels through this
/// read-only path.
///
/// An empty channel set is a 409 carrying [`crate::InviteExhausted`]'s
/// shared wording — the same refusal [`redeem_enrollment_endpoint`] gives
/// for the identical condition, since one `channels_for` read cannot tell
/// "never issued here" apart from "already fully redeemed".
async fn preview_enrollment(
    State(host): State<Arc<Host>>,
    Form(form): Form<PreviewForm>,
) -> Response {
    let payload = match decode_enroll_or_400(&form.enroll) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    let channels =
        match crate::invites::channels_for(&junto_home, &payload.invite_token, &payload.email) {
            Ok(channels) => channels,
            Err(err) => return identity_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
        };
    if channels.is_empty() {
        return identity_error(StatusCode::CONFLICT, crate::InviteExhausted.to_string());
    }

    let mut dtos = Vec::with_capacity(channels.len());
    for channel in channels {
        let name = project(&host, &channel)
            .await
            .ok()
            .and_then(|(_, view, _)| view.name);
        dtos.push(PreviewChannelDto { id: channel, name });
    }

    axum::Json(EnrollPreviewDto {
        email: payload.email.clone(),
        display_name: payload.display_name.clone(),
        fingerprint: crate::identity::fingerprint(&payload.public_key),
        transport_fingerprint: crate::identity::fingerprint(&payload.transport_public_key),
        channels: dtos,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{ApprovalRequirement, GateStatus, Member, Standing};
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    #[test]
    fn parse_mcp_rows_pairs_names_with_urls_and_drops_blanks() {
        let pairs = vec![
            ("name".into(), "Reviewer".into()),
            ("mcp_name".into(), "junto".into()),
            ("mcp_url".into(), "http://127.0.0.1:1727/mcp".into()),
            // A blank trailing row (the add-row) is dropped.
            ("mcp_name".into(), "  ".into()),
            ("mcp_url".into(), "".into()),
            // A half-filled row (name but no url) is dropped too.
            ("mcp_name".into(), "orphan".into()),
            ("mcp_url".into(), "".into()),
        ];
        let servers = parse_mcp_rows(&pairs);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "junto");
        assert_eq!(servers[0].url, "http://127.0.0.1:1727/mcp");
    }

    #[test]
    fn slugify_makes_a_stable_kebab_slug() {
        assert_eq!(slugify("Security Reviewer"), "security-reviewer");
        assert_eq!(slugify("  Doc Writer!! "), "doc-writer");
        assert_eq!(slugify("***"), "");
    }

    #[tokio::test]
    async fn error_responses_become_a_styled_page() {
        // A plain-text refusal (what authorize_human_write returns) becomes a
        // styled HTML page that preserves the message and offers a way back.
        let raw = (StatusCode::FORBIDDEN, "you aren’t a member of this channel").into_response();
        let pretty = prettify_errors(raw).await;
        assert_eq!(pretty.status(), StatusCode::FORBIDDEN);
        let is_html = pretty
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        assert!(is_html, "rendered as HTML");
        let bytes = axum::body::to_bytes(pretty.into_body(), 64 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("you aren’t a member of this channel"));
        assert!(html.contains("Go back"));
    }

    #[tokio::test]
    async fn success_responses_pass_through_unchanged() {
        let raw = Html("<h1>ok</h1>").into_response();
        let same = prettify_errors(raw).await;
        assert_eq!(same.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(same.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"<h1>ok</h1>");
    }

    /// A test host: one fresh git repo, the member-code store in its own temp
    /// dir, a channel founded by the repo's git user (the web-write author —
    /// a member, so human-surface acts authorize without any code), and one
    /// entry by a granted "Bot" member.
    struct WebFixture {
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        channel: ChannelId,
        target: EntryId,
    }

    async fn host_with_entry(payload: EntryPayload) -> WebFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let member_home = tempfile::tempdir().expect("member home");
        let host = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        // The founder is the git user, so web writes are member writes.
        let founder = Member::human("Web User", "web@example.com");
        let opened = host
            .open_channel(None, "web-test", founder.clone(), None)
            .await
            .expect("open channel");
        let channel = opened.id;
        // The bot is a granted member: its entry projects (Provisional /
        // Pending) so the page renders an act form for it.
        host.add_member(
            "web-test",
            &founder,
            Member::agent("Bot", "bot@example.com"),
            None,
            None,
        )
        .await
        .expect("add bot");
        let target = EntryId::new();
        let ledger = host.ledger_for(dir.path()).await.expect("ledger");
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                signature: None,
                id: target,
                channel,
                author: Member::agent("Bot", "bot@example.com"),
                timestamp: Timestamp::now(),
                payload,
            })
            .await
            .expect("append");
        WebFixture {
            _dirs: vec![dir, member_home],
            host,
            channel,
            target,
        }
    }

    fn assertion() -> EntryPayload {
        EntryPayload::Assertion {
            statement: "claim".into(),
            rationale: "because".into(),
            provenance: vec![],
            frame: None,
        }
    }

    fn proposal() -> EntryPayload {
        EntryPayload::Proposal {
            action: "merge it".into(),
            rationale: "ready".into(),
            provenance: vec![],
            frame: None,
            requirement: ApprovalRequirement::Count(1),
            kind: None,
        }
    }

    fn session_started() -> EntryPayload {
        EntryPayload::SessionStarted {
            intent: "do the work".into(),
        }
    }

    #[tokio::test]
    async fn interrupt_errors_when_idle_and_redirects_when_live() {
        let fx = host_with_entry(session_started()).await;

        // No live turn for the session → interrupting is a BAD_REQUEST.
        let idle = interrupt_session(
            State(fx.host.clone()),
            Path(("web-test".into(), fx.target.to_string())),
        )
        .await;
        assert_eq!(idle.status(), StatusCode::BAD_REQUEST);

        // With a live feed (receiver kept alive), the interrupt is delivered
        // and the card redirects.
        let _rx = fx
            .host
            .live()
            .begin(fx.host.clone(), "web-test".into(), fx.target, true);
        let live = interrupt_session(
            State(fx.host.clone()),
            Path(("web-test".into(), fx.target.to_string())),
        )
        .await;
        assert_eq!(live.status(), StatusCode::SEE_OTHER);
    }

    async fn post_act(
        host: Arc<Host>,
        channel: String,
        entry: String,
        act: &str,
        rationale: &str,
    ) -> Response {
        post_act_with(host, channel, entry, act, rationale, "").await
    }

    async fn post_act_with(
        host: Arc<Host>,
        channel: String,
        entry: String,
        act: &str,
        rationale: &str,
        back: &str,
    ) -> Response {
        verify(
            State(host),
            Path((channel, entry, act.to_string())),
            Form(ActForm {
                rationale: rationale.into(),
                back: back.into(),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn web_ratify_moves_standing_with_git_author() {
        // No member code anywhere in the form: the host derives the author
        // and authorizes membership itself (Host::authorize_human_write).
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked it",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let resolution = fx.host.resolve(&fx.channel.to_string()).await.unwrap();
        let Resolution::Resolved { ledger, id, .. } = resolution else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(matches!(
            view.standing(&fx.target),
            Some(Standing::Ratified)
        ));
        // The act was authored as the repo's configured git user.
        let act_entry = view
            .entries
            .iter()
            .find(|e| matches!(e.payload, EntryPayload::Ratification { .. }))
            .expect("ratification recorded");
        assert_eq!(act_entry.author.email, "web@example.com");
    }

    #[tokio::test]
    async fn web_approve_opens_the_gate() {
        let fx = host_with_entry(proposal()).await;
        let response = post_act(
            fx.host.clone(),
            fx.channel.to_string(),
            fx.target.to_string(),
            "approve",
            "lgtm",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let Resolution::Resolved { ledger, id, .. } =
            fx.host.resolve(&fx.channel.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(matches!(
            view.gate_status(&fx.target),
            Some(GateStatus::Approved)
        ));
    }

    #[tokio::test]
    async fn empty_rationale_is_refused() {
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "  ",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn cross_kind_acts_are_refused() {
        // Approving an assertion (it's not a proposal) is a 400, not a
        // silently-dangling act.
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "approve",
            "r",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_acts_are_404() {
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "yolo",
            "r",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn back_path_honored_and_hostile_back_ignored() {
        // A board-originated act returns to "/"; no cookie of any kind is
        // set (the member-code remember-cookie is gone with the code itself).
        let fx = host_with_entry(assertion()).await;
        let response = post_act_with(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked",
            "/",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            Some("/")
        );
        assert!(
            response.headers().get(header::SET_COOKIE).is_none(),
            "no cookie machinery on the human surface"
        );

        // A hostile back path is ignored in favor of the channel page.
        let fx2 = host_with_entry(proposal()).await;
        let response = post_act_with(
            fx2.host.clone(),
            "web-test".into(),
            fx2.target.to_string(),
            "approve",
            "lgtm",
            "//evil.example.com",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert!(
            location.starts_with("/channels/"),
            "hostile back ignored: {location}"
        );
    }

    #[tokio::test]
    async fn web_writes_auto_sync_to_origin() {
        // The fixture repo gets a bare `origin`; a web act must land there
        // without anyone running sync_channel (the terminal-less human has no
        // way to). The sync is a background task, so poll briefly.
        let fx = host_with_entry(assertion()).await;
        let repo = fx._dirs[0].path().to_path_buf();
        let bare = tempfile::tempdir().expect("bare dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q", "--bare"])
                .current_dir(bare.path())
                .status()
                .expect("git init --bare")
                .success()
        );
        assert!(
            StdCommand::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    &bare.path().display().to_string()
                ])
                .current_dir(&repo)
                .status()
                .expect("git remote add")
                .success()
        );

        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // The web author's ref appears on the remote, carrying the act.
        let expected = format!("refs/junto/{}/web%40example%2Ecom", fx.channel);
        let mut synced = false;
        for _ in 0..50 {
            let out = StdCommand::new("git")
                .args(["ls-remote", "origin", &expected])
                .current_dir(&repo)
                .output()
                .expect("git ls-remote");
            if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
                synced = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(synced, "auto-sync pushed {expected} to origin");
    }

    /// The response body as text, for asserting on rendered pages.
    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    #[tokio::test]
    async fn setup_repo_runs_init_and_lands_on_the_ambient_channel() {
        // The terminal-less `junto init`: registering from the form leaves
        // the repo wired (harness config, binding) with its ambient channel
        // open, and the redirect lands on that channel's page.
        let home = crate::host::test_home::HomeGuard::new();
        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let host = Host::from_registry(home.path().to_path_buf());

        let response = setup_repo(
            State(host.clone()),
            Form(SetupRepoForm {
                path: repo.path().display().to_string(),
                channel: "ambient-test".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("redirect")
            .to_string();
        assert!(location.starts_with("/channels/"), "{location}");

        // The substrate is registered and the ambient channel is open with
        // the git user as founder; the harness wiring exists in the repo.
        let Resolution::Resolved { id, ledger, .. } = host.resolve("ambient-test").await.unwrap()
        else {
            panic!("ambient channel resolves");
        };
        assert_eq!(location, format!("/channels/{id}"));
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(view.party[0].email, "web@example.com");
        assert!(repo.path().join(".mcp.json").exists());

        // A non-repo path is refused with the reason.
        let not_a_repo = tempfile::tempdir().expect("dir");
        let response = setup_repo(
            State(host),
            Form(SetupRepoForm {
                path: not_a_repo.path().display().to_string(),
                channel: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let page = body_text(response).await;
        assert!(page.contains("not a git repository"), "{page}");
    }

    #[tokio::test]
    async fn launch_runs_a_turn_and_records_the_session() {
        // End to end with a stubbed harness (docs/adr/0023): launch from the
        // form → SessionStarted appears → the background turn finishes → a
        // memo artifact + done state land, and the harness session id is
        // remembered for --resume.
        let home = crate::host::test_home::HomeGuard::new();
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = if cfg!(windows) {
            let path = stub_dir.path().join("stub.cmd");
            std::fs::write(
                &path,
                "@echo {\"type\":\"result\",\"subtype\":\"success\",\"result\":\"stub work \
                 complete\",\"session_id\":\"h-stub-1\",\"is_error\":false}\r\n",
            )
            .expect("write stub");
            path
        } else {
            let path = stub_dir.path().join("stub.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"stub \
                 work complete\",\"session_id\":\"h-stub-1\",\"is_error\":false}'\n",
            )
            .expect("write stub");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod stub");
            }
            path
        };
        // Safe-enough: env mutation is serialized by the HomeGuard's lock.
        unsafe { std::env::set_var("JUNTO_HARNESS_CMD", &stub) };

        let fx = host_with_entry(assertion()).await;
        // The harness member must be in the Party for its entries to project.
        let founder = Member::human("Web User", "web@example.com");
        fx.host
            .add_member(
                "web-test",
                &founder,
                crate::launch::harness_member(),
                None,
                None,
            )
            .await
            .expect("grant the harness membership");
        // A workspace repo for the session to run in.
        let workspace = tempfile::tempdir().expect("workspace");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(workspace.path())
                .status()
                .expect("git init")
                .success()
        );

        let response = launch_session(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LaunchForm {
                intent: "do the stub thing".into(),
                workspace: workspace.path().display().to_string(),
                agent: String::new(),
                mode: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // Poll the projection until the background turn lands.
        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("web-test").await.unwrap()
        else {
            panic!("channel resolves");
        };
        let mut done_session = None;
        for _ in 0..100 {
            let view = ledger.lock().await.project(&id).await.unwrap();
            if let Some((session_id, session)) = view
                .sessions
                .iter()
                .find(|(_, s)| s.state == junto_kernel::SessionState::Done)
            {
                assert!(
                    !session.artifacts.is_empty(),
                    "the turn attached at least the result memo"
                );
                done_session = Some(*session_id);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if done_session.is_none() {
            let view = ledger.lock().await.project(&id).await.unwrap();
            for entry in &view.entries {
                eprintln!("entry: {:?}", entry.payload);
            }
            for (sid, s) in &view.sessions {
                eprintln!("session {sid}: {:?}", s.state);
            }
        }
        let session = done_session.expect("session reached done");

        // The memo artifact carries the stub's result and a file:// + sha256
        // provenance; the harness session id is remembered for --resume.
        let view = ledger.lock().await.project(&id).await.unwrap();
        let memo = view
            .entries
            .iter()
            .find_map(|entry| match &entry.payload {
                junto_kernel::EntryPayload::ArtifactAttached {
                    target,
                    kind,
                    description,
                    provenance,
                } if *target == session && kind == "memo" => {
                    Some((description.clone(), provenance.clone()))
                }
                _ => None,
            })
            .expect("memo artifact recorded");
        assert!(memo.0.contains("stub work complete"), "{}", memo.0);
        assert!(memo.1[0].uri.as_str().starts_with("file:///"));
        assert!(memo.1[0].digest.is_some());
        assert_eq!(
            crate::launch::harness_session_for(home.path(), &session)
                .unwrap()
                .as_deref(),
            Some("h-stub-1")
        );

        unsafe { std::env::remove_var("JUNTO_HARNESS_CMD") };
    }

    #[tokio::test]
    async fn launch_auto_grants_the_harness_when_the_founder_starts_work() {
        // A fresh channel has only its founder in the Party; the founder
        // starting work should bring the harness in (a founder-authored
        // MemberAdded) rather than reject the launch — the new-channel
        // papercut (docs/adr/0017).
        let _home = crate::host::test_home::HomeGuard::new();
        let stub_dir = tempfile::tempdir().expect("stub dir");
        let stub = if cfg!(windows) {
            let path = stub_dir.path().join("stub.cmd");
            std::fs::write(
                &path,
                "@echo {\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\",\
                 \"session_id\":\"h-grant-1\",\"is_error\":false}\r\n",
            )
            .expect("write stub");
            path
        } else {
            let path = stub_dir.path().join("stub.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\",\
                 \"session_id\":\"h-grant-1\",\"is_error\":false}'\n",
            )
            .expect("write stub");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod stub");
            }
            path
        };
        unsafe { std::env::set_var("JUNTO_HARNESS_CMD", &stub) };

        // host_with_entry founds "web-test" by the git user (web@example.com)
        // and grants only a "Bot" — the harness is deliberately not a member.
        let fx = host_with_entry(assertion()).await;
        let harness = crate::launch::harness_member();
        let workspace = tempfile::tempdir().expect("workspace");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(workspace.path())
                .status()
                .expect("git init")
                .success()
        );

        let response = launch_session(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LaunchForm {
                intent: "start in a fresh channel".into(),
                workspace: workspace.path().display().to_string(),
                agent: String::new(),
                mode: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // The harness is now a member, and the grant was authored by the
        // founder (the git user), not by the agent itself.
        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("web-test").await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(
            view.party.iter().any(|m| m.email == harness.email),
            "the harness was auto-granted membership"
        );
        let granted_by = view.entries.iter().find_map(|entry| match &entry.payload {
            junto_kernel::EntryPayload::MemberAdded { member } if member.email == harness.email => {
                Some(entry.author.email.clone())
            }
            _ => None,
        });
        assert_eq!(granted_by.as_deref(), Some("web@example.com"));

        unsafe { std::env::remove_var("JUNTO_HARNESS_CMD") };
    }

    #[tokio::test]
    async fn view_artifact_serves_content_and_refuses_paths_outside_the_store() {
        let home = crate::host::test_home::HomeGuard::new();
        let fx = host_with_entry(assertion()).await;
        let Resolution::Resolved { ledger, .. } = fx.host.resolve("web-test").await.unwrap() else {
            panic!("channel resolves");
        };
        let session = EntryId::new();

        // An ArtifactAttached entry (authored by a member so it projects)
        // whose provenance points at `uri`.
        let attach = |id: EntryId, uri: String| LedgerEntry {
            signature: None,
            id,
            channel: fx.channel,
            author: Member::agent("Bot", "bot@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::ArtifactAttached {
                target: session,
                kind: "memo".into(),
                description: "snippet…".into(),
                provenance: vec![junto_kernel::ProvenanceRef::new(
                    junto_kernel::Uri::new(uri).expect("uri"),
                )],
            },
        };

        // A real artifact file under this machine's artifacts root.
        let dir = home.path().join("artifacts").join(session.to_string());
        std::fs::create_dir_all(&dir).expect("artifact dir");
        let file = dir.join("turn-1-result.md");
        std::fs::write(&file, "the FULL agent output\nsecond line").expect("write artifact");
        let good = EntryId::new();
        let good_uri = format!("file:///{}", file.display().to_string().replace('\\', "/"));
        ledger
            .lock()
            .await
            .append(attach(good, good_uri))
            .await
            .unwrap();

        // Happy path: the whole content comes back, not just a snippet.
        let response = view_artifact(
            State(fx.host.clone()),
            Path(("web-test".into(), good.to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("the FULL agent output"), "{text}");
        assert!(text.contains("second line"), "the full body, not a snippet");

        // Guard: a file outside the artifacts root is refused even with a
        // valid entry (entries can arrive by sync carrying any path).
        let secret = home.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();
        let evil = EntryId::new();
        let evil_uri = format!(
            "file:///{}",
            secret.display().to_string().replace('\\', "/")
        );
        ledger
            .lock()
            .await
            .append(attach(evil, evil_uri))
            .await
            .unwrap();
        let response = view_artifact(
            State(fx.host.clone()),
            Path(("web-test".into(), evil.to_string())),
        )
        .await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "must not serve a path outside the artifacts root"
        );
    }

    #[tokio::test]
    async fn close_then_reopen_round_trips() {
        let fx = host_with_entry(assertion()).await;

        // Close: the channel projects closed and demotes in summaries.
        let response = close_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LifecycleForm {
                rationale: "inquiry finished".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("web-test").await.unwrap()
        else {
            panic!("closed channel still resolves by name");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(view.closed);
        let summary = fx
            .host
            .inventory()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap();
        assert!(summary.closed);

        // Closing again is a conflict.
        let response = close_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LifecycleForm {
                rationale: "again".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // Reopen: back in the working set.
        let response = reopen_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(LifecycleForm {
                rationale: "it resumed".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(!view.closed);
    }

    #[tokio::test]
    async fn rename_supersedes_the_genesis_binding() {
        let fx = host_with_entry(assertion()).await;
        let response = rename_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(RenameForm {
                name: "better-name".into(),
                rationale: "the inquiry sharpened".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        // The new name resolves; the old one no longer does.
        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("better-name").await.unwrap()
        else {
            panic!("renamed channel resolves by its new name");
        };
        assert_eq!(id, fx.channel);
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(view.name.as_deref(), Some("better-name"));
        assert!(matches!(
            fx.host.resolve("web-test").await.unwrap(),
            Resolution::NotFound
        ));

        // Renaming onto a taken name is a conflict.
        let response = open_channel(
            State(fx.host.clone()),
            Form(OpenChannelForm {
                name: "other".into(),
                repo: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let response = rename_channel(
            State(fx.host.clone()),
            Path("better-name".into()),
            Form(RenameForm {
                name: "other".into(),
                rationale: "collide".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn the_index_form_opens_a_channel() {
        // The human-surface counterpart of the open_channel tool: post a
        // name, the host picks its only substrate and the git user as
        // founder, and the redirect lands on the new channel's page.
        let fx = host_with_entry(assertion()).await;
        let response = open_channel(
            State(fx.host.clone()),
            Form(OpenChannelForm {
                name: "fresh-inquiry".into(),
                repo: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("redirect target")
            .to_string();
        assert!(location.starts_with("/channels/"), "{location}");

        // The channel resolves by name, founded by the repo's git user.
        let Resolution::Resolved { ledger, id, .. } =
            fx.host.resolve("fresh-inquiry").await.unwrap()
        else {
            panic!("the opened channel resolves by name");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(view.name.as_deref(), Some("fresh-inquiry"));
        assert_eq!(view.party[0].email, "web@example.com");
        // The redirect targeted exactly this channel.
        assert_eq!(location, format!("/channels/{id}"));
    }

    #[tokio::test]
    async fn a_taken_name_is_a_conflict() {
        let fx = host_with_entry(assertion()).await;
        let response = open_channel(
            State(fx.host.clone()),
            Form(OpenChannelForm {
                name: "web-test".into(), // the fixture already opened this
                repo: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let page = body_text(response).await;
        assert!(page.contains("web-test"), "{page}");
    }

    #[tokio::test]
    async fn diverge_form_opens_a_linked_child() {
        let fx = host_with_entry(assertion()).await;
        let response = diverge_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(DivergeForm {
                child_name: "ui-side-quest".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("redirect target")
            .to_string();
        assert!(location.starts_with("/channels/"), "{location}");

        // The child resolves, founded by the git user, with a DivergedFrom
        // edge back to the parent.
        let Resolution::Resolved { ledger, id, .. } =
            fx.host.resolve("ui-side-quest").await.unwrap()
        else {
            panic!("the side-quest resolves by name");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(view.party[0].email, "web@example.com");
        assert!(
            view.lineage.iter().any(
                |edge| edge.relation == junto_kernel::LineageRelation::Diverge
                    && edge.direction == junto_kernel::LineageDirection::Incoming
                    && edge.other == fx.channel
            ),
            "the child records it diverged from the parent"
        );
    }

    #[tokio::test]
    async fn converge_form_closes_the_source() {
        let fx = host_with_entry(assertion()).await;
        // A second open channel in the same substrate, founded by the same git
        // user so the human write is a member write.
        let founder = Member::human("Web User", "web@example.com");
        let target = fx
            .host
            .open_channel(None, "target", founder, None)
            .await
            .unwrap()
            .id;
        let response = converge_channel(
            State(fx.host.clone()),
            Path("web-test".into()),
            Form(ConvergeForm {
                target: target.to_string(),
                rationale: "merged the side-quest".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let Resolution::Resolved { ledger, id, .. } = fx.host.resolve("web-test").await.unwrap()
        else {
            panic!("the source resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(view.closed, "convergence closed the source");
    }

    #[tokio::test]
    async fn non_member_git_identity_is_forbidden() {
        // The human-surface guardrail that remains: the git-config author
        // must be in the channel's Party. A repo whose git user was never
        // granted membership gets a clear refusal and nothing is appended.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Stranger"), ("user.email", "x@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let member_home = tempfile::tempdir().expect("member home");
        let host = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        // Founded by someone else: the git user is not in the Party.
        let founder = Member::human("Founder", "founder@example.com");
        let opened = host
            .open_channel(None, "web-test", founder, None)
            .await
            .expect("open channel");
        let target = EntryId::new();
        let ledger = host.ledger_for(dir.path()).await.expect("ledger");
        ledger
            .lock()
            .await
            .append(LedgerEntry {
                signature: None,
                id: target,
                channel: opened.id,
                author: Member::human("Founder", "founder@example.com"),
                timestamp: Timestamp::now(),
                payload: assertion(),
            })
            .await
            .expect("append");

        let response = post_act(
            host.clone(),
            "web-test".into(),
            target.to_string(),
            "ratify",
            "drive-by",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let page = body_text(response).await;
        assert!(
            page.contains("x@example.com"),
            "the refusal names the non-member identity: {page}"
        );

        // Nothing was appended: the assertion is still provisional.
        let view = ledger.lock().await.project(&opened.id).await.unwrap();
        assert!(matches!(
            view.standing(&target),
            Some(Standing::Provisional)
        ));
    }

    #[test]
    fn provisional_assertions_render_a_verification_form() {
        // Rendering-level check: the page carries the ratify form for a
        // provisional assertion (the web write surface's entry point).
        let channel = ChannelId::new();
        let entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: Member::agent("Bot", "bot@example.com"),
            timestamp: Timestamp::now(),
            payload: assertion(),
        };
        let view = ChannelView {
            name: Some("web-test".into()),
            standings: std::iter::once((entry.id, junto_kernel::Standing::Provisional)).collect(),
            gate_status: Default::default(),
            gate_executions: Default::default(),
            entries: vec![entry.clone()],
            party: Vec::new(),
            keyring: Default::default(),
            unrecognized: Default::default(),
            unverified: Default::default(),
            sessions: Default::default(),
            closed: false,
            lineage: Vec::new(),
        };
        let html = crate::render::channel_html(
            &[],
            "web-test",
            &channel,
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains(&format!("/channels/{channel}/entries/{}/ratify", entry.id)));
        assert!(html.contains("name=\"rationale\""));
        // The act-feedback enhancement ships with every page: a submitted act
        // shows "recording…" instead of reading as a dead click.
        assert!(
            html.contains("recording\\u2026"),
            "act feedback script present"
        );
    }

    #[tokio::test]
    async fn view_json_flags_an_unverified_entry() {
        // An entry signed by a key that does not match its author's grant
        // projects as unverified; assert the JSON carries unverified: true
        // for it and false for a properly signed neighbour.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let member_home = tempfile::tempdir().expect("member home");
        let host = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        let opened = host
            .open_channel(None, "web-test", founder.clone(), None)
            .await
            .expect("open channel");
        let channel = opened.id;
        let bot = Member::agent("Bot", "bot@example.com");
        let granted_key = junto_kernel::SigningKey::from_secret_bytes([21; 32]);
        let stray_key = junto_kernel::SigningKey::from_secret_bytes([22; 32]);
        host.add_member(
            "web-test",
            &founder,
            bot.clone(),
            Some(granted_key.public_key()),
            None,
        )
        .await
        .expect("add bot with a granted key");

        let ledger = host.ledger_for(dir.path()).await.expect("ledger");
        let mut verified_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: bot.clone(),
            timestamp: Timestamp::now(),
            payload: assertion(),
        };
        verified_entry
            .sign(&granted_key)
            .expect("sign with the granted key");
        let verified_id = verified_entry.id;
        ledger
            .lock()
            .await
            .append(verified_entry)
            .await
            .expect("append the properly signed entry");

        let mut unverified_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: bot.clone(),
            timestamp: Timestamp::now(),
            payload: assertion(),
        };
        unverified_entry
            .sign(&stray_key)
            .expect("sign with a key that was never granted");
        let unverified_id = unverified_entry.id;
        ledger
            .lock()
            .await
            .append(unverified_entry)
            .await
            .expect("append the wrongly signed entry");

        let response = channel_view_json(State(host.clone()), Path("web-test".into())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        let entries = json["entries"].as_array().expect("entries array");
        let find = |id: EntryId| {
            entries
                .iter()
                .find(|e| e["id"] == id.to_string())
                .unwrap_or_else(|| panic!("entry {id} present in the JSON: {entries:?}"))
        };
        assert_eq!(
            find(unverified_id)["unverified"],
            true,
            "signed by an ungranted key"
        );
        assert_eq!(
            find(verified_id)["unverified"],
            false,
            "signed by the author's granted key"
        );
    }

    /// A test host whose founder has two grants on their own email: one
    /// from the channel genesis (no transport key — pre-Task-16), one an
    /// explicit second device carrying both halves. The founder granting
    /// their own second device is `Host::add_member`'s self-grant path
    /// (`docs/adr/0033`): a founder's second device is admitted exactly
    /// like anyone else's.
    struct KeysFixture {
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        founder: Member,
        device_key: junto_kernel::PublicKey,
        device_transport_key: junto_kernel::PublicKey,
    }

    async fn host_with_two_grants() -> KeysFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let member_home = tempfile::tempdir().expect("member home");
        let host = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        host.open_channel(None, "keys-test", founder.clone(), None)
            .await
            .expect("open channel");
        let device_key = junto_kernel::SigningKey::from_secret_bytes([9; 32]);
        let device_transport_key = junto_kernel::SigningKey::from_secret_bytes([10; 32]);
        host.add_member(
            "keys-test",
            &founder,
            founder.clone(),
            Some(device_key.public_key()),
            Some(device_transport_key.public_key()),
        )
        .await
        .expect("grant a second device to the founder's own email");
        KeysFixture {
            _dirs: vec![dir, member_home],
            host,
            founder,
            device_key: device_key.public_key(),
            device_transport_key: device_transport_key.public_key(),
        }
    }

    /// Retire a grant by appending the founder-authored `Park` that
    /// `retire-device` itself constructs (main.rs) — the only way
    /// `KeyGrant::retired_at` is ever set.
    async fn park_grant(host: &Host, founder: &Member, target: EntryId) {
        let (id, _view, _substrate) = match project_fresh(host, "keys-test").await {
            Ok(projected) => projected,
            Err(_) => panic!("'keys-test' must project"),
        };
        let mut park = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: founder.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Park {
                target,
                rationale: "device lost".into(),
            },
        };
        host.sign_entry(&mut park);
        let Resolution::Resolved { ledger, .. } = host.resolve("keys-test").await.unwrap() else {
            panic!("channel 'keys-test' resolves");
        };
        ledger.lock().await.append(park).await.unwrap();
    }

    #[tokio::test]
    async fn keys_json_lists_devices_by_fingerprint_and_never_the_whole_key() {
        let fx = host_with_two_grants().await;
        let (_id, view, _substrate) = match project_fresh(&fx.host, "keys-test").await {
            Ok(projected) => projected,
            Err(_) => panic!("'keys-test' must project"),
        };
        let grants = view
            .keyring
            .get("web@example.com")
            .expect("founder has grants");
        assert_eq!(grants.len(), 2, "genesis grant + the explicit device grant");
        let genesis_key = grants[0].key.clone();
        let genesis_grant_id = grants[0].granted_by;

        // Retire the FIRST (genesis) grant only — one active, one retired,
        // and — because it is the physically-first keyring entry — a
        // retirement-based sort (e.g. `sort_by_key(|g| g.retired_at)`,
        // where `None < Some` moves the still-active device grant ahead of
        // it) would visibly reorder `devices` relative to the untouched
        // keyring order this test pins below.
        park_grant(&fx.host, &fx.founder, genesis_grant_id).await;

        let response = keys_json(State(fx.host.clone()), Path("keys-test".into())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;

        let genesis_fingerprint = crate::identity::fingerprint(&genesis_key);
        let device_fingerprint = crate::identity::fingerprint(&fx.device_key);
        let transport_fingerprint = crate::identity::fingerprint(&fx.device_transport_key);
        assert!(
            body.contains(&genesis_fingerprint),
            "genesis grant's fingerprint present: {body}"
        );
        assert!(
            body.contains(&device_fingerprint),
            "device grant's fingerprint present: {body}"
        );
        assert!(
            body.contains(&transport_fingerprint),
            "device transport fingerprint present: {body}"
        );

        // Never the whole public key — the security property this endpoint
        // exists to uphold.
        assert!(
            !body.contains(genesis_key.as_str()),
            "the genesis signing key must never appear whole: {body}"
        );
        assert!(
            !body.contains(fx.device_key.as_str()),
            "the device signing key must never appear whole: {body}"
        );
        assert!(
            !body.contains(fx.device_transport_key.as_str()),
            "the device transport key must never appear whole: {body}"
        );

        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(json["viewer_is_founder"], true);
        assert_eq!(json["viewer_email"], "web@example.com");
        assert_eq!(
            json["members"][0]["email"], json["founder_email"],
            "party order — founder first — must not be re-sorted"
        );
        let founder_dto = json["members"]
            .as_array()
            .expect("members array")
            .iter()
            .find(|m| m["email"] == "web@example.com")
            .expect("founder present in members");
        let devices = founder_dto["devices"].as_array().expect("devices array");
        assert_eq!(devices.len(), 2);

        // Keyring order (genesis grant, then the explicit device) must
        // survive — not sorted by fingerprint or by retirement.
        assert_eq!(devices[0]["fingerprint"], genesis_fingerprint);
        assert!(
            !devices[0]["retired_at"].is_null(),
            "the genesis grant was retired: {devices:?}"
        );
        assert_eq!(devices[1]["fingerprint"], device_fingerprint);
        assert!(
            devices[1]["retired_at"].is_null(),
            "the device grant is still active: {devices:?}"
        );

        // The genesis grant predates transport keys (Task 16): its
        // transport_fingerprint is null — never a fallback to its own
        // signing fingerprint, never "".
        assert_eq!(devices[0]["transport_fingerprint"], serde_json::Value::Null);
        assert_ne!(
            devices[0]["transport_fingerprint"], devices[0]["fingerprint"],
            "a null transport_fingerprint must never fall back to the signing fingerprint"
        );
        assert_eq!(devices[1]["transport_fingerprint"], transport_fingerprint);

        let retired_count = devices
            .iter()
            .filter(|d| !d["retired_at"].is_null())
            .count();
        assert_eq!(retired_count, 1, "exactly one grant retired: {devices:?}");
        assert_eq!(
            founder_dto["revoked"], false,
            "one grant is still active, so the email is not revoked"
        );
    }

    #[tokio::test]
    async fn keys_json_marks_an_email_revoked_only_when_every_grant_is_retired() {
        let fx = host_with_two_grants().await;
        let (_id, view, _substrate) = match project_fresh(&fx.host, "keys-test").await {
            Ok(projected) => projected,
            Err(_) => panic!("'keys-test' must project"),
        };
        let grants = view
            .keyring
            .get("web@example.com")
            .expect("founder has grants")
            .clone();
        assert_eq!(grants.len(), 2, "genesis grant + the explicit device grant");

        // Park both grants — every device retired.
        for grant in &grants {
            park_grant(&fx.host, &fx.founder, grant.granted_by).await;
        }

        let response = keys_json(State(fx.host.clone()), Path("keys-test".into())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        let founder_dto = json["members"]
            .as_array()
            .expect("members array")
            .iter()
            .find(|m| m["email"] == "web@example.com")
            .expect("a fully revoked member stays in the party (ADR 0035)");
        assert_eq!(founder_dto["revoked"], true);
        assert_eq!(
            founder_dto["devices"]
                .as_array()
                .expect("devices array")
                .len(),
            2,
            "both retired grants still list as devices"
        );
    }

    #[tokio::test]
    async fn keys_json_gives_a_keyless_member_empty_devices() {
        // A member with no grants at all — appended directly on the ledger
        // so no key is ever minted for them (unlike `Host::add_member`,
        // which mints one for an agent when none is supplied).
        let fx = host_with_two_grants().await;
        let (id, _view, _substrate) = match project_fresh(&fx.host, "keys-test").await {
            Ok(projected) => projected,
            Err(_) => panic!("'keys-test' must project"),
        };
        let mut add = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: id,
            author: fx.founder.clone(),
            timestamp: Timestamp::now(),
            payload: EntryPayload::MemberAdded {
                member: Member::human("Keyless", "keyless@example.com"),
            },
        };
        fx.host.sign_entry(&mut add);
        let Resolution::Resolved { ledger, .. } = fx.host.resolve("keys-test").await.unwrap()
        else {
            panic!("channel 'keys-test' resolves");
        };
        ledger.lock().await.append(add).await.unwrap();

        let response = keys_json(State(fx.host.clone()), Path("keys-test".into())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        // Two members now (founder + keyless): a real ordering pin — party
        // order, founder first — not the vacuous single-element compare a
        // one-member fixture would give.
        assert_eq!(json["members"][0]["email"], "web@example.com");
        assert_eq!(json["members"][1]["email"], "keyless@example.com");
        let keyless_dto = json["members"]
            .as_array()
            .expect("members array")
            .iter()
            .find(|m| m["email"] == "keyless@example.com")
            .expect("the keyless member is still in the party");
        assert_eq!(
            keyless_dto["devices"]
                .as_array()
                .expect("devices array")
                .len(),
            0
        );
        assert_eq!(keyless_dto["revoked"], false);
    }

    #[tokio::test]
    async fn keys_json_404s_for_an_unknown_channel_like_the_other_json_routes() {
        let fx = host_with_entry(assertion()).await;
        let keys_response = keys_json(State(fx.host.clone()), Path("no-such-channel".into())).await;
        let view_response =
            channel_view_json(State(fx.host.clone()), Path("no-such-channel".into())).await;
        assert_eq!(keys_response.status(), view_response.status());
        assert_eq!(keys_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn keys_json_answers_200_with_no_viewer_identity_when_git_config_is_unset() {
        // Env mutation (GIT_CONFIG_GLOBAL/SYSTEM below) is process-global —
        // serialized by the same lock every other env-mutating test in this
        // file holds.
        let _home = crate::host::test_home::HomeGuard::new();
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        // Deliberately no local user.name/user.email — and the global and
        // system config files are pointed at paths that do not exist, so
        // this machine's own git identity cannot leak in and mask what this
        // test means to exercise.
        let nonexistent = dir.path().join("no-such-gitconfig");
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &nonexistent);
            std::env::set_var("GIT_CONFIG_SYSTEM", &nonexistent);
        }
        let member_home = tempfile::tempdir().expect("member home");
        let host = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        host.open_channel(
            None,
            "no-identity-test",
            Member::human("Nobody", "nobody@example.com"),
            None,
        )
        .await
        .expect("open channel");

        // Confirm the isolation actually worked, or the rest of this test
        // would exercise nothing.
        assert!(
            crate::host::git_user(dir.path()).is_err(),
            "git config must be genuinely unreachable for this test to mean anything"
        );

        let response = keys_json(State(host.clone()), Path("no-identity-test".into())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert!(
            json.get("viewer_email").is_some(),
            "viewer_email must be present-and-null, not omitted: {body}"
        );
        assert_eq!(json["viewer_email"], serde_json::Value::Null);
        assert_eq!(json["viewer_is_founder"], false);

        unsafe {
            std::env::remove_var("GIT_CONFIG_GLOBAL");
            std::env::remove_var("GIT_CONFIG_SYSTEM");
        }
    }

    /// A test host with one repo whose git user is "Web User"
    /// <web@example.com>: two channels that user founded ("chan-a",
    /// "chan-b"), and one ("not-mine") founded by someone else — so
    /// `mint_invite`'s per-channel authority check has something to
    /// refuse. `member_home` is pinned to the same directory `HomeGuard`
    /// points `JUNTO_HOME` at, so `Host`'s own party-keying and
    /// `mint_invite`'s direct `crate::host::junto_home()` calls
    /// (`invites.toml`) land in one place.
    struct InviteFixture {
        _home: crate::host::test_home::HomeGuard,
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
    }

    async fn invite_fixture() -> InviteFixture {
        let home = crate::host::test_home::HomeGuard::new();
        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let host = Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        host.open_channel(None, "chan-a", founder.clone(), None)
            .await
            .expect("open chan-a");
        host.open_channel(None, "chan-b", founder.clone(), None)
            .await
            .expect("open chan-b");
        host.open_channel(
            None,
            "not-mine",
            Member::human("Other", "other@example.com"),
            None,
        )
        .await
        .expect("open not-mine");
        InviteFixture {
            _home: home,
            _dirs: vec![repo],
            host,
        }
    }

    #[tokio::test]
    async fn post_invites_mints_one_token_covering_every_channel() {
        let fx = invite_fixture().await;
        let Resolution::Resolved { id: chan_a_id, .. } = fx.host.resolve("chan-a").await.unwrap()
        else {
            panic!("chan-a resolves");
        };
        let Resolution::Resolved { id: chan_b_id, .. } = fx.host.resolve("chan-b").await.unwrap()
        else {
            panic!("chan-b resolves");
        };
        let mut expected = vec![chan_a_id.to_string(), chan_b_id.to_string()];
        expected.sort();

        let pairs = vec![
            ("member".to_string(), "eve@example.com".to_string()),
            ("channel".to_string(), "chan-a".to_string()),
            ("channel".to_string(), "chan-b".to_string()),
        ];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let url = json["url"].as_str().expect("url string").to_string();
        let decoded = crate::enroll::decode_invite(&url).expect("decodes");
        assert_eq!(decoded.member_email, "eve@example.com");
        let mut decoded_channels = decoded.channels.clone();
        decoded_channels.sort();
        assert_eq!(decoded_channels, expected);

        let mut json_channels: Vec<String> = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        json_channels.sort();
        assert_eq!(json_channels, expected);

        let junto_home = crate::host::junto_home().unwrap();
        let mut covers =
            crate::invites::channels_for(&junto_home, &decoded.invite_token, "eve@example.com")
                .unwrap();
        covers.sort();
        assert_eq!(covers, expected, "channels_for recovers both channels");
    }

    #[tokio::test]
    async fn post_invites_issues_nothing_when_one_channel_is_not_the_callers() {
        let fx = invite_fixture().await;
        let pairs = vec![
            ("member".to_string(), "eve@example.com".to_string()),
            ("channel".to_string(), "chan-a".to_string()),
            ("channel".to_string(), "not-mine".to_string()),
        ];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_text(response).await;
        assert!(
            body.contains("not-mine"),
            "names the offending channel: {body}"
        );

        // All-or-nothing: nothing was issued, not even for "chan-a", which
        // the caller *did* found — so `invites.toml` was never written.
        let junto_home = crate::host::junto_home().unwrap();
        assert!(
            !junto_home.join("invites.toml").exists(),
            "no record issued for any channel"
        );
    }

    #[tokio::test]
    async fn post_invites_refuses_an_empty_channel_set() {
        let fx = invite_fixture().await;
        let pairs = vec![("member".to_string(), "eve@example.com".to_string())];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let junto_home = crate::host::junto_home().unwrap();
        assert!(
            !junto_home.join("invites.toml").exists(),
            "nothing issued for an empty channel set"
        );
    }

    #[tokio::test]
    async fn post_invites_refuses_a_blank_member() {
        let fx = invite_fixture().await;
        let pairs = vec![
            ("member".to_string(), "   ".to_string()),
            ("channel".to_string(), "chan-a".to_string()),
        ];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let junto_home = crate::host::junto_home().unwrap();
        assert!(
            !junto_home.join("invites.toml").exists(),
            "nothing issued for a blank member"
        );
    }

    #[tokio::test]
    async fn post_invites_refuses_an_oversized_member() {
        let fx = invite_fixture().await;
        let pairs = vec![
            (
                "member".to_string(),
                "x".repeat(crate::enroll::MAX_FIELD_CHARS + 1),
            ),
            ("channel".to_string(), "chan-a".to_string()),
        ];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let junto_home = crate::host::junto_home().unwrap();
        assert!(
            !junto_home.join("invites.toml").exists(),
            "nothing issued for a member over the field-length cap"
        );
    }

    #[tokio::test]
    async fn post_invites_dedupes_two_spellings_of_the_same_channel() {
        let fx = invite_fixture().await;
        let Resolution::Resolved { id: chan_a_id, .. } = fx.host.resolve("chan-a").await.unwrap()
        else {
            panic!("chan-a resolves");
        };

        // One channel, named twice: once by its display name, once by the
        // canonical id that name resolves to.
        let pairs = vec![
            ("member".to_string(), "eve@example.com".to_string()),
            ("channel".to_string(), "chan-a".to_string()),
            ("channel".to_string(), chan_a_id.to_string()),
        ];
        let response = mint_invite(State(fx.host.clone()), Form(pairs)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let channels = json["channels"].as_array().expect("channels array");
        assert_eq!(
            channels.len(),
            1,
            "two spellings of one channel collapse to one record: {channels:?}"
        );
        assert_eq!(channels[0], chan_a_id.to_string());

        let url = json["url"].as_str().expect("url string");
        let decoded = crate::enroll::decode_invite(url).expect("decodes");
        assert_eq!(decoded.channels.len(), 1);

        let junto_home = crate::host::junto_home().unwrap();
        let covers =
            crate::invites::channels_for(&junto_home, &decoded.invite_token, "eve@example.com")
                .unwrap();
        assert_eq!(covers.len(), 1, "one issued record, not two: {covers:?}");
    }

    /// A freshly encoded, currently-valid invite for `email` — built
    /// directly (not through `mint_invite`), since these tests exercise
    /// only the enroll side.
    fn valid_invite_payload(email: &str) -> crate::enroll::InvitePayload {
        crate::enroll::InvitePayload {
            v: crate::enroll::PAYLOAD_VERSION,
            invite_token: crate::enroll::mint_invite_token(),
            member_email: email.to_string(),
            channels: vec!["chan-x".to_string()],
            expires_at: Timestamp::now().as_millis() + 60_000,
        }
    }

    #[tokio::test]
    async fn post_devices_enroll_mints_for_the_invites_email_and_echoes_only_the_public_half() {
        let _home = crate::host::test_home::HomeGuard::new();
        let invite = valid_invite_payload("eve@example.com");
        let url = crate::enroll::encode_invite(&invite).expect("encodes");

        let response = enroll_device(Form(EnrollForm {
            invite: url,
            name: Some("Eve's Laptop".to_string()),
        }))
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        let enrolled_url = json["url"].as_str().expect("url string");
        let decoded = crate::enroll::decode_enroll(enrolled_url).expect("decodes");
        assert_eq!(decoded.email, "eve@example.com");
        assert_eq!(json["email"], "eve@example.com");

        let junto_home = crate::host::junto_home().unwrap();
        let key = crate::keys::signing_key(&junto_home, "eve@example.com").expect("minted");
        let transport = crate::keys::transport_key(&junto_home, "eve@example.com").expect("minted");
        assert_eq!(
            json["fingerprint"],
            crate::identity::fingerprint(&key.public_key())
        );
        assert_eq!(
            json["transport_fingerprint"],
            crate::identity::fingerprint(&transport.public_key())
        );

        // Never the 64-hex secret from keys.toml, for either key.
        assert!(
            !body.contains(&key.to_secret_hex()),
            "signing secret leaked: {body}"
        );
        assert!(
            !body.contains(&transport.to_secret_hex()),
            "transport secret leaked: {body}"
        );

        let keys_toml = std::fs::read_to_string(junto_home.join("keys.toml")).unwrap();
        assert!(
            keys_toml.contains("eve@example.com"),
            "keys.toml now holds the invite's email: {keys_toml}"
        );
    }

    #[tokio::test]
    async fn post_devices_enroll_ignores_an_email_supplied_by_the_caller() {
        let _home = crate::host::test_home::HomeGuard::new();
        let invite = valid_invite_payload("eve@example.com");
        let url = crate::enroll::encode_invite(&invite).expect("encodes");

        // A raw urlencoded body — the real extraction path a browser POST
        // takes — carrying an extra `email` key `EnrollForm` has no field
        // for. Serde's default (non-`deny_unknown_fields`) struct
        // deserialization silently drops it; this pins that behavior.
        let body = format!("invite={url}&email=attacker@x.com&name=Eve");
        let request = axum::http::Request::builder()
            .method("POST")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from(body))
            .expect("request");
        let Form(form) =
            <Form<EnrollForm> as axum::extract::FromRequest<()>>::from_request(request, &())
                .await
                .expect("deserializes despite the extra `email` field");

        let response = enroll_device(Form(form)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("valid json");
        assert_eq!(
            json["email"], "eve@example.com",
            "the invite's email wins, never the caller-supplied one"
        );
    }

    #[tokio::test]
    async fn post_devices_enroll_refuses_an_expired_invite_before_minting() {
        let _home = crate::host::test_home::HomeGuard::new();
        let mut invite = valid_invite_payload("eve@example.com");
        invite.expires_at = Timestamp::now().as_millis() - crate::enroll::MAX_INVITE_TTL_MS;
        let url = crate::enroll::encode_invite(&invite).expect("encodes");

        let response = enroll_device(Form(EnrollForm {
            invite: url,
            name: None,
        }))
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_text(response).await;
        assert!(body.to_lowercase().contains("expir"), "{body}");

        let junto_home = crate::host::junto_home().unwrap();
        assert!(
            !junto_home.join("keys.toml").exists(),
            "no key minted for an expired invite"
        );
    }

    /// A test host with one repo whose git user founds two channels
    /// ("chan-a", "chan-b") — an invite token covering both, issued
    /// straight through `crate::invites::issue` (not `mint_invite`, since
    /// these tests exercise only the redemption side), plus the device
    /// keypair a redemption enrolls.
    struct RedeemFixture {
        _home: crate::host::test_home::HomeGuard,
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        chan_a: ChannelId,
        chan_b: ChannelId,
    }

    async fn redeem_fixture() -> RedeemFixture {
        let home = crate::host::test_home::HomeGuard::new();
        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let host = Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        let chan_a = host
            .open_channel(None, "chan-a", founder.clone(), None)
            .await
            .expect("open chan-a")
            .id;
        let chan_b = host
            .open_channel(None, "chan-b", founder.clone(), None)
            .await
            .expect("open chan-b")
            .id;
        RedeemFixture {
            _home: home,
            _dirs: vec![repo],
            host,
            chan_a,
            chan_b,
        }
    }

    /// Issue `token` (covering `fx`'s two channels) for `email`, and encode
    /// the matching enroll payload for `key`/`transport_key`.
    fn issue_two_channel_invite(
        fx: &RedeemFixture,
        email: &str,
        key: &junto_kernel::SigningKey,
        transport_key: &junto_kernel::SigningKey,
    ) -> String {
        let junto_home = crate::host::junto_home().unwrap();
        let token = crate::enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + 60_000;
        for channel in [fx.chan_a.to_string(), fx.chan_b.to_string()] {
            crate::invites::issue(&junto_home, &token, email, &channel, expires_at).expect("issue");
        }
        let payload = crate::enroll::EnrollPayload {
            v: crate::enroll::PAYLOAD_VERSION,
            invite_token: token,
            email: email.to_string(),
            display_name: "Eve".to_string(),
            public_key: key.public_key(),
            transport_public_key: transport_key.public_key(),
            expires_at,
        };
        crate::enroll::encode_enroll(&payload).expect("encodes")
    }

    #[tokio::test]
    async fn post_members_grants_every_channel_and_reports_each() {
        let fx = redeem_fixture().await;
        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let url = issue_two_channel_invite(&fx, "eve@example.com", &key, &transport_key);

        let response = redeem_enrollment_endpoint(
            State(fx.host.clone()),
            Form(RedeemForm {
                enroll: url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        let outcomes = json["outcomes"].as_array().expect("outcomes array");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "granted", "{outcome:?}");
        }

        for channel in [fx.chan_a, fx.chan_b] {
            let (_, view, _) = project(&fx.host, &channel.to_string())
                .await
                .expect("projects");
            let grants = view.keyring.get("eve@example.com").expect("eve has grants");
            assert!(
                grants
                    .iter()
                    .any(|g| g.key == key.public_key() && g.retired_at.is_none()),
                "channel {channel} holds eve's key as an ACTIVE grant: {grants:?}"
            );
        }
    }

    /// Set up a token whose invite record for every channel in `fx` is a
    /// stale, already-consumed duplicate — the one way
    /// `RedeemOutcome::InviteAlreadyUsed` is reachable now that
    /// `AlreadyAMember` is decided first (see `main.rs`'s
    /// `a_stale_consumed_duplicate_record_reports_invite_already_used`,
    /// mirrored here for both of this fixture's channels): a fresh
    /// duplicate keeps `channels_for` naming the channel, but `consume`
    /// lands on the stale, already-used record first. Returns the encoded
    /// enroll code for "eve@example.com".
    fn stale_duplicate_invite_enroll_url(fx: &RedeemFixture) -> String {
        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let junto_home = crate::host::junto_home().unwrap();
        let token = crate::enroll::mint_invite_token();
        let expires_at = Timestamp::now().as_millis() + 60_000;
        for channel in [fx.chan_a.to_string(), fx.chan_b.to_string()] {
            crate::invites::issue(&junto_home, &token, "eve@example.com", &channel, expires_at)
                .expect("issue first record");
            crate::invites::issue(&junto_home, &token, "eve@example.com", &channel, expires_at)
                .expect("issue duplicate record");
            assert!(matches!(
                crate::invites::consume(&junto_home, &token, "eve@example.com", &channel)
                    .expect("consume"),
                crate::invites::Consumed::Ok
            ));
        }
        let payload = crate::enroll::EnrollPayload {
            v: crate::enroll::PAYLOAD_VERSION,
            invite_token: token,
            email: "eve@example.com".to_string(),
            display_name: "Eve".to_string(),
            public_key: key.public_key(),
            transport_public_key: transport_key.public_key(),
            expires_at,
        };
        crate::enroll::encode_enroll(&payload).expect("encodes")
    }

    #[tokio::test]
    async fn post_members_returns_409_with_outcomes_when_nothing_could_be_granted() {
        let fx = redeem_fixture().await;
        let url = stale_duplicate_invite_enroll_url(&fx);

        let response = redeem_enrollment_endpoint(
            State(fx.host.clone()),
            Form(RedeemForm {
                enroll: url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        let outcomes = json["outcomes"].as_array().expect("outcomes array");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "invite_already_used", "{outcome:?}");
        }
    }

    /// Finding 1 (review round 1): `prettify_errors` used to rewrite the
    /// body of EVERY error-status response whose `Content-Type` was not
    /// `text/html` into a styled HTML page — silently discarding a JSON
    /// endpoint's structured error body (here, `/members`'s 409
    /// `{"outcomes":[…]}`, exactly what the brief requires that body to
    /// carry). `prettify_errors` is the ONLY transformation `router()`
    /// applies on top of a handler's own `Response` (routing/extraction do
    /// not touch an already-built body), so piping the handler's raw 409
    /// through it — precisely what `.layer(axum::middleware::
    /// map_response(prettify_errors))` does for every live request —
    /// reproduces the real over-the-wire response without a full HTTP
    /// client.
    #[tokio::test]
    async fn post_members_409_json_body_survives_the_error_prettifying_layer() {
        let fx = redeem_fixture().await;
        let url = stale_duplicate_invite_enroll_url(&fx);

        let response = redeem_enrollment_endpoint(
            State(fx.host.clone()),
            Form(RedeemForm {
                enroll: url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let response = prettify_errors(response).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            content_type.as_deref(),
            Some("application/json"),
            "the layer must not have rewritten a JSON error body"
        );
        let body = body_text(response).await;
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("still valid JSON after prettify_errors");
        let outcomes = json["outcomes"].as_array().expect("outcomes array");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "invite_already_used", "{outcome:?}");
        }
    }

    /// A raw HTTP/1.1 POST over a plain `TcpStream` — no HTTP-client
    /// dependency, just the `tokio::net`/`io-util` primitives already in
    /// this crate's dependency tree. Returns (status, content-type, body).
    async fn http_post(
        addr: std::net::SocketAddr,
        path: &str,
        body: &str,
    ) -> (u16, String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/x-www-form-urlencoded\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("read response");
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").expect("header/body split");
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("numeric status code");
        let content_type = head
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default();
        (status, content_type, body.to_string())
    }

    /// Serve `host`'s real router on an ephemeral localhost port — mirrors
    /// `live_ws.rs`'s own `serve_router` test helper (same crate, same
    /// pattern), so this needs no `tower`/HTTP-client dependency.
    async fn serve_router(host: Arc<Host>) -> std::net::SocketAddr {
        let app = router(host);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        addr
    }

    /// Finding 1 residual (review round 2): the handler+layer composition
    /// test above pins the layer FUNCTION but not the WIRING — re-pointing
    /// the route, or moving prettification off `map_response`, would leave
    /// that test green while the wire body is destroyed again. This drives
    /// the actual composed `router()` over a real socket, so it only
    /// passes when `/members` is wired through the real `prettify_errors`
    /// layer end to end.
    #[tokio::test]
    async fn post_members_409_json_survives_the_real_router_over_the_wire() {
        let fx = redeem_fixture().await;
        let url = stale_duplicate_invite_enroll_url(&fx);
        let addr = serve_router(fx.host.clone()).await;

        let body = format!("enroll={url}&kind=human");
        let (status, content_type, body) = http_post(addr, "/members", &body).await;

        assert_eq!(status, 409);
        assert_eq!(content_type, "application/json");
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("still valid JSON over the wire");
        let outcomes = json["outcomes"].as_array().expect("outcomes array");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "invite_already_used", "{outcome:?}");
        }
    }

    /// Finding 2 (review round 1): `redeem_enrollment` `?`s on
    /// `host::junto_home()` and `invites::channels_for` before it can
    /// even reach its own empty-set `bail!` — so a genuinely unreadable
    /// or corrupt invite store must NOT be reported as 409 (that status
    /// means "the whole set was already used or not ours", a normal
    /// operator-facing condition, not an I/O failure). Compared against
    /// [`preview_enrollment`], which already classifies the identical
    /// failure as `internal`.
    #[tokio::test]
    async fn post_members_500s_on_a_genuine_invite_store_failure_not_409() {
        let home = crate::host::test_home::HomeGuard::new();
        // Corrupt invites.toml so `channels_for`'s `load` fails to parse,
        // rather than legitimately reporting an empty (exhausted) set.
        std::fs::write(home.path().join("invites.toml"), "not valid toml {{{").unwrap();
        let host = Host::fixed_with_member_home(vec![], Some(home.path().to_path_buf()));

        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let payload = crate::enroll::EnrollPayload {
            v: crate::enroll::PAYLOAD_VERSION,
            invite_token: crate::enroll::mint_invite_token(),
            email: "eve@example.com".to_string(),
            display_name: "Eve".to_string(),
            public_key: key.public_key(),
            transport_public_key: transport_key.public_key(),
            expires_at: Timestamp::now().as_millis() + 60_000,
        };
        let url = crate::enroll::encode_enroll(&payload).expect("encodes");

        let response = redeem_enrollment_endpoint(
            State(host),
            Form(RedeemForm {
                enroll: url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn post_members_refuses_a_missing_or_unknown_kind() {
        let fx = redeem_fixture().await;
        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let url = issue_two_channel_invite(&fx, "eve@example.com", &key, &transport_key);

        // kind absent from the wire: `#[serde(default)]` lets extraction
        // succeed (kind == ""), so it is the *handler* that refuses with
        // 400 — never axum's own 422 form-rejection, and never defaulted
        // to "human" (ADR 0035).
        let body = format!("enroll={url}");
        let request = axum::http::Request::builder()
            .method("POST")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(axum::body::Body::from(body))
            .expect("request");
        let Form(form) =
            <Form<RedeemForm> as axum::extract::FromRequest<()>>::from_request(request, &())
                .await
                .expect("deserializes; kind defaults to empty, not an error");
        assert_eq!(form.kind, "");
        let response = redeem_enrollment_endpoint(State(fx.host.clone()), Form(form)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // kind="person": the handler itself refuses, never defaulting.
        let response = redeem_enrollment_endpoint(
            State(fx.host.clone()),
            Form(RedeemForm {
                enroll: url,
                kind: "person".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_devices_preview_shows_the_channel_set_without_consuming_it() {
        let fx = redeem_fixture().await;
        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let url = issue_two_channel_invite(&fx, "eve@example.com", &key, &transport_key);

        let response = preview_enrollment(
            State(fx.host.clone()),
            Form(PreviewForm {
                enroll: url.clone(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(json["email"], "eve@example.com");
        assert_eq!(
            json["fingerprint"],
            crate::identity::fingerprint(&key.public_key())
        );
        // Finding 2 (final fix wave): the founder's only integrity check
        // on the transport half — pins that it is present and correct,
        // not merely that the endpoint still answers 200.
        assert_eq!(
            json["transport_fingerprint"],
            crate::identity::fingerprint(&transport_key.public_key())
        );
        let mut channels: Vec<String> = json["channels"]
            .as_array()
            .expect("channels array")
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect();
        channels.sort();
        let mut expected = vec![fx.chan_a.to_string(), fx.chan_b.to_string()];
        expected.sort();
        assert_eq!(channels, expected, "{body}");

        // Redeem for real: BOTH channels still grantable — the preview
        // consumed nothing.
        let redeemed = redeem_enrollment_endpoint(
            State(fx.host.clone()),
            Form(RedeemForm {
                enroll: url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(redeemed.status(), StatusCode::OK);
        let redeemed_json: serde_json::Value =
            serde_json::from_str(&body_text(redeemed).await).expect("valid json");
        let outcomes = redeemed_json["outcomes"].as_array().expect("outcomes");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "granted", "{outcome:?}");
        }
    }

    #[tokio::test]
    async fn post_devices_preview_409s_when_the_token_covers_nothing() {
        let _home = crate::host::test_home::HomeGuard::new();
        let key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let transport_key = junto_kernel::SigningKey::from_secret_bytes([8; 32]);
        let payload = crate::enroll::EnrollPayload {
            v: crate::enroll::PAYLOAD_VERSION,
            invite_token: crate::enroll::mint_invite_token(),
            email: "eve@example.com".to_string(),
            display_name: "Eve".to_string(),
            public_key: key.public_key(),
            transport_public_key: transport_key.public_key(),
            expires_at: Timestamp::now().as_millis() + 60_000,
        };
        let url = crate::enroll::encode_enroll(&payload).expect("encodes");

        let host = Host::fixed_with_member_home(vec![], Some(_home.path().to_path_buf()));
        let response = preview_enrollment(State(host), Form(PreviewForm { enroll: url })).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_text(response).await;
        assert!(body.contains("never"), "{body}");
        assert!(body.contains("already"), "{body}");
    }

    /// A test host with one repo whose git user "Web User" founds one
    /// channel and grants "Carol" two device grants (two distinct keys) —
    /// the roster [`retire_device`]/[`revoke_member`] act on.
    struct RevokeFixture {
        _home: crate::host::test_home::HomeGuard,
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        channel: ChannelId,
        founder: Member,
        member: Member,
    }

    async fn revoke_fixture() -> RevokeFixture {
        let home = crate::host::test_home::HomeGuard::new();
        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Web User"), ("user.email", "web@example.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let host = Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        let channel = host
            .open_channel(None, "acme", founder.clone(), None)
            .await
            .expect("open channel")
            .id;
        let member = Member::human("Carol", "carol@example.com");
        let key_a = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let key_b = junto_kernel::SigningKey::from_secret_bytes([9; 32]);
        host.add_member(
            "acme",
            &founder,
            member.clone(),
            Some(key_a.public_key()),
            None,
        )
        .await
        .expect("add carol device a");
        host.add_member(
            "acme",
            &founder,
            member.clone(),
            Some(key_b.public_key()),
            None,
        )
        .await
        .expect("add carol device b");
        RevokeFixture {
            _home: home,
            _dirs: vec![repo],
            host,
            channel,
            founder,
            member,
        }
    }

    #[tokio::test]
    async fn post_revoke_parks_every_active_grant_and_leaves_the_member_in_the_party() {
        let fx = revoke_fixture().await;
        let response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.member.email.clone())),
            Form(RationaleForm {
                rationale: "leaving the team".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("valid json");
        assert_eq!(json["parked"], 2);

        let (_, view, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let grants = view
            .keyring
            .get(&fx.member.email)
            .expect("carol has grants");
        assert_eq!(grants.len(), 2);
        assert!(grants.iter().all(|g| g.retired_at.is_some()), "{grants:?}");
        assert!(
            view.party.iter().any(|m| m.email == fx.member.email),
            "carol stays in the party (ADR 0035)"
        );
    }

    #[tokio::test]
    async fn post_revoke_refuses_the_founder_and_an_empty_rationale() {
        let fx = revoke_fixture().await;
        let (_, view_before, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let entries_before = view_before.entries.len();

        let founder_response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.founder.email.clone())),
            Form(RationaleForm {
                rationale: "rotating".to_string(),
            }),
        )
        .await;
        assert_eq!(founder_response.status(), StatusCode::BAD_REQUEST);

        let empty_rationale_response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.member.email.clone())),
            Form(RationaleForm {
                rationale: "   ".to_string(),
            }),
        )
        .await;
        assert_eq!(empty_rationale_response.status(), StatusCode::BAD_REQUEST);

        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before,
            "nothing appended in either case"
        );
    }

    /// Finding 4a (review round 1): `revoke_fixture`'s target had two
    /// ACTIVE grants and none retired, so a naive "every grant for this
    /// email" (ignoring `retired_at`) reported the identical `{"parked":2}`
    /// as the real `grants_to_park` — the "every ACTIVE grant" contract was
    /// unobservable. Retiring one grant first, then revoking, makes the
    /// two implementations diverge: only 1 more grant to park, exactly 1
    /// new `Park` entry.
    #[tokio::test]
    async fn post_revoke_parks_only_active_grants_leaving_already_retired_ones_alone() {
        let fx = revoke_fixture().await;
        let (_, view, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let grants = view
            .keyring
            .get(&fx.member.email)
            .expect("carol has grants");
        assert_eq!(grants.len(), 2);
        let already_retired = grants[0].granted_by;

        let retire_response = retire_device(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), already_retired.to_string())),
            Form(RationaleForm {
                rationale: "retiring device a ahead of the revoke".to_string(),
            }),
        )
        .await;
        assert_eq!(retire_response.status(), StatusCode::OK);

        let (_, view_before, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let entries_before = view_before.entries.len();

        let response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.member.email.clone())),
            Form(RationaleForm {
                rationale: "leaving the team".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("valid json");
        assert_eq!(
            json["parked"], 1,
            "only the still-active grant is parked, not the already-retired one"
        );

        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before + 1,
            "exactly one new Park entry — none for the already-retired grant"
        );
    }

    /// Finding 4b (review round 1): `revoke_member`'s `targets.is_empty()`
    /// 400 branch had no test — revoking the same member twice leaves
    /// nothing to park the second time.
    #[tokio::test]
    async fn post_revoke_refuses_a_member_with_no_active_grants() {
        let fx = revoke_fixture().await;
        let first_response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.member.email.clone())),
            Form(RationaleForm {
                rationale: "leaving the team".to_string(),
            }),
        )
        .await;
        assert_eq!(first_response.status(), StatusCode::OK);

        let (_, view_before, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let entries_before = view_before.entries.len();

        let second_response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.member.email.clone())),
            Form(RationaleForm {
                rationale: "again".to_string(),
            }),
        )
        .await;
        assert_eq!(second_response.status(), StatusCode::BAD_REQUEST);

        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before,
            "nothing appended for a member with no active grants"
        );
    }

    #[tokio::test]
    async fn post_retire_parks_one_grant_and_refuses_an_already_retired_one() {
        let fx = revoke_fixture().await;
        let (_, view, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let grants = view
            .keyring
            .get(&fx.member.email)
            .expect("carol has grants");
        assert_eq!(grants.len(), 2);
        let first = grants[0].granted_by;
        let second = grants[1].granted_by;

        let response = retire_device(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), first.to_string())),
            Form(RationaleForm {
                rationale: "rotating device".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("valid json");
        assert_eq!(json["parked"], 1);

        let (_, view, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let grants = view.keyring.get(&fx.member.email).expect("has grants");
        let retired = grants
            .iter()
            .find(|g| g.granted_by == first)
            .expect("first grant present");
        assert!(retired.retired_at.is_some());
        let still_active = grants
            .iter()
            .find(|g| g.granted_by == second)
            .expect("second grant present");
        assert!(still_active.retired_at.is_none());
        let entries_before = view.entries.len();

        let second_response = retire_device(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), first.to_string())),
            Form(RationaleForm {
                rationale: "again".to_string(),
            }),
        )
        .await;
        assert_eq!(second_response.status(), StatusCode::CONFLICT);
        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before,
            "no second Park appended"
        );
    }

    /// Finding 5 (review round 1): `retire_device`'s unknown-grant 404 and
    /// its empty-rationale refusal had no tests.
    #[tokio::test]
    async fn post_retire_refuses_an_unknown_grant_and_an_empty_rationale() {
        let fx = revoke_fixture().await;
        let (_, view_before, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let entries_before = view_before.entries.len();

        // A fresh, never-issued entry id names no grant in this channel.
        let unknown = EntryId::new();
        let unknown_response = retire_device(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), unknown.to_string())),
            Form(RationaleForm {
                rationale: "rotating".to_string(),
            }),
        )
        .await;
        assert_eq!(unknown_response.status(), StatusCode::NOT_FOUND);

        let grant = view_before
            .keyring
            .get(&fx.member.email)
            .and_then(|grants| grants.first())
            .expect("carol has a grant")
            .granted_by;
        let empty_rationale_response = retire_device(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), grant.to_string())),
            Form(RationaleForm {
                rationale: "   ".to_string(),
            }),
        )
        .await;
        assert_eq!(empty_rationale_response.status(), StatusCode::BAD_REQUEST);

        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before,
            "nothing appended in either case"
        );
    }

    /// A host whose git identity is `caller_name`/`caller_email` — never
    /// this channel's founder — with `target_name`/`target_email` added as
    /// a party member holding one grant. Shared by [`retire_device`]'s and
    /// [`revoke_member`]'s "caller is not the founder" tests: the founder-
    /// authority check they both call through `crate::identity::
    /// require_founder` must refuse no matter which act it gates. Passing
    /// the same name/email for both parameters (as the retire test does)
    /// makes the caller its own target, exactly the original single-Carol
    /// setup.
    struct NonFounderCallerFixture {
        _home: crate::host::test_home::HomeGuard,
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        channel: ChannelId,
        target: Member,
        grant: EntryId,
    }

    async fn non_founder_caller_fixture(
        caller_name: &str,
        caller_email: &str,
        target_name: &str,
        target_email: &str,
    ) -> NonFounderCallerFixture {
        let home = crate::host::test_home::HomeGuard::new();
        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", caller_name), ("user.email", caller_email)] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let host = Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let founder = Member::human("Web User", "web@example.com");
        let channel = host
            .open_channel(None, "acme", founder.clone(), None)
            .await
            .expect("open channel")
            .id;
        let target = Member::human(target_name, target_email);
        let key = junto_kernel::SigningKey::from_secret_bytes([11; 32]);
        host.add_member(
            "acme",
            &founder,
            target.clone(),
            Some(key.public_key()),
            None,
        )
        .await
        .expect("add target");
        let (_, view, _) = project(&host, &channel.to_string())
            .await
            .expect("projects");
        let grant = view
            .keyring
            .get(&target.email)
            .and_then(|grants| grants.first())
            .expect("target has a grant")
            .granted_by;
        NonFounderCallerFixture {
            _home: home,
            _dirs: vec![repo],
            host,
            channel,
            target,
            grant,
        }
    }

    #[tokio::test]
    async fn post_retire_refuses_a_caller_who_is_not_the_founder() {
        // Carol is both the caller and the retired grant's own owner.
        let fx =
            non_founder_caller_fixture("Carol", "carol@example.com", "Carol", "carol@example.com")
                .await;
        let response = retire_device(
            State(fx.host),
            Path((fx.channel.to_string(), fx.grant.to_string())),
            Form(RationaleForm {
                rationale: "trying to retire someone else's grant".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Finding 3 (review round 1): `revoke_member`'s founder gate had no
    /// test of its own — `revoke_fixture` always calls as the founder, so
    /// deleting `require_founder`'s check there left every Task 10 test
    /// green. Dave is a resolvable git identity that is neither the
    /// founder nor even a party member; Carol is a third member with an
    /// active grant.
    #[tokio::test]
    async fn post_revoke_refuses_a_caller_who_is_not_the_founder() {
        let fx =
            non_founder_caller_fixture("Dave", "dave@example.com", "Carol", "carol@example.com")
                .await;
        let (_, view_before, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        let entries_before = view_before.entries.len();

        let response = revoke_member(
            State(fx.host.clone()),
            Path((fx.channel.to_string(), fx.target.email.clone())),
            Form(RationaleForm {
                rationale: "trying to revoke someone else's grant".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let (_, view_after, _) = project(&fx.host, &fx.channel.to_string())
            .await
            .expect("projects");
        assert_eq!(
            view_after.entries.len(),
            entries_before,
            "nothing appended for a refused caller"
        );
    }

    /// Sign `assertion()` as `author` with `key`, stamped `timestamp`, and
    /// append it directly to `ledger` — the shape a device's own process
    /// would append with, bypassing [`Host::sign_entry`] entirely (which
    /// signs with the *caller's* home, never a remote device's).
    async fn sign_and_append(
        ledger: &crate::host::SharedLedger,
        channel: ChannelId,
        author: &Member,
        key: &junto_kernel::SigningKey,
        timestamp: Timestamp,
    ) -> EntryId {
        let mut entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: author.clone(),
            timestamp,
            payload: assertion(),
        };
        entry.sign(key).expect("sign with the device's own key");
        let id = entry.id;
        ledger.lock().await.append(entry).await.expect("append");
        id
    }

    /// The device-key-enrollment plan's end-to-end proof (Task 15): two
    /// `junto_home`s on one machine standing in for two machines — the
    /// founder's own, and the enrolling device's — sharing one substrate
    /// repo, exactly as they would share a real git remote. Walks the
    /// whole three-step exchange over HTTP (`POST /invites` → `POST
    /// /devices/enroll` against the SECOND home → `POST /members`), then
    /// both post-grant lifecycle acts (`retire-device`, `revoke-member`),
    /// asserting every consequence the brief names: (a) both invited
    /// channels' keyrings hold the new grant, (b) an entry signed by the
    /// device's own key projects verified in both, (c) the founder's
    /// machine never minted or held a key for the enrolled email, (d)
    /// `keys.json` shows the enrolled device with the right fingerprints
    /// on both channels, (e) retiring one channel's grant leaves the
    /// other channel's grant still verifying entries signed after that
    /// retirement, and (f) revoking the member on that other channel
    /// unrecognizes a later entry from the same device while an earlier
    /// one keeps the standing it already had.
    ///
    /// Every entry "signed by the device" here is appended straight to
    /// the shared ledger with the device's own [`junto_kernel::
    /// SigningKey`] — never through [`Host::sign_entry`], which would
    /// sign with the *founder's* home and could never touch the device's
    /// actual key. Timestamps that must land on a specific side of a
    /// retirement/revocation cutoff are constructed explicitly
    /// ([`Timestamp::from_millis`]) rather than left to `Timestamp::now`'s
    /// wall-clock resolution — real time could tie the cutoff on a fast
    /// machine, and the fold's `>` comparison is what is under test here,
    /// not the system clock.
    #[tokio::test]
    async fn pairing_end_to_end() {
        let home = crate::host::test_home::HomeGuard::new();
        let device_home = tempfile::tempdir().expect("device home");

        let repo = tempfile::tempdir().expect("repo dir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(repo.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [
            ("user.name", "Founder"),
            ("user.email", "founder@example.com"),
        ] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }

        let host = Host::fixed_with_member_home(
            vec![repo.path().to_path_buf()],
            Some(home.path().to_path_buf()),
        );
        let founder = Member::human("Founder", "founder@example.com");
        let chan_a = host
            .open_channel(None, "chan-a", founder.clone(), None)
            .await
            .expect("open chan-a")
            .id;
        let chan_b = host
            .open_channel(None, "chan-b", founder.clone(), None)
            .await
            .expect("open chan-b")
            .id;

        // --- leg 1: `POST /invites`, on the founder's own home, for both
        // channels at once.
        let pairs = vec![
            ("member".to_string(), "eve@example.com".to_string()),
            ("channel".to_string(), "chan-a".to_string()),
            ("channel".to_string(), "chan-b".to_string()),
        ];
        let invite_response = mint_invite(State(host.clone()), Form(pairs)).await;
        assert_eq!(invite_response.status(), StatusCode::OK);
        let invite_json: serde_json::Value =
            serde_json::from_str(&body_text(invite_response).await).expect("valid json");
        let invite_url = invite_json["url"].as_str().expect("invite url").to_string();

        // --- leg 2: `POST /devices/enroll`, against the SECOND home — the
        // device's own machine, standing in for a real second one.
        unsafe { std::env::set_var("JUNTO_HOME", device_home.path()) };
        let enroll_response = enroll_device(Form(EnrollForm {
            invite: invite_url,
            name: Some("Eve's Laptop".to_string()),
        }))
        .await;
        assert_eq!(enroll_response.status(), StatusCode::OK);
        let enroll_json: serde_json::Value =
            serde_json::from_str(&body_text(enroll_response).await).expect("valid json");
        let enroll_url = enroll_json["url"].as_str().expect("enroll url").to_string();
        // The device's own keys, minted on ITS home just now — never the
        // founder's — so the test can sign as the device below, and (c)
        // below can prove the founder's own store never learned them.
        let device_signing_key = crate::keys::signing_key(device_home.path(), "eve@example.com")
            .expect("the device's own signing key");
        let device_transport_key =
            crate::keys::transport_key(device_home.path(), "eve@example.com")
                .expect("the device's own transport key");

        // --- leg 3: `POST /members`, back on the founder's own home — the
        // founder is the one with authority to grant.
        unsafe { std::env::set_var("JUNTO_HOME", home.path()) };
        let redeem_response = redeem_enrollment_endpoint(
            State(host.clone()),
            Form(RedeemForm {
                enroll: enroll_url,
                kind: "human".to_string(),
            }),
        )
        .await;
        assert_eq!(redeem_response.status(), StatusCode::OK);
        let redeem_json: serde_json::Value =
            serde_json::from_str(&body_text(redeem_response).await).expect("valid json");
        let outcomes = redeem_json["outcomes"].as_array().expect("outcomes array");
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in outcomes {
            assert_eq!(outcome["result"], "granted", "{outcome:?}");
        }

        // (a) both channels' keyrings hold the new grant.
        let (_, view_a, _) = project_fresh(&host, "chan-a")
            .await
            .expect("chan-a projects");
        let (_, view_b, _) = project_fresh(&host, "chan-b")
            .await
            .expect("chan-b projects");
        let active_grant = |view: &ChannelView| {
            view.keyring
                .get("eve@example.com")
                .and_then(|grants| grants.iter().find(|g| g.retired_at.is_none()))
                .cloned()
                .expect("an active grant for eve")
        };
        let grant_a = active_grant(&view_a);
        let grant_b = active_grant(&view_b);
        assert_eq!(grant_a.key, device_signing_key.public_key());
        assert_eq!(grant_b.key, device_signing_key.public_key());
        assert_eq!(
            grant_a.transport_key.as_ref(),
            Some(&device_transport_key.public_key())
        );
        assert_eq!(
            grant_b.transport_key.as_ref(),
            Some(&device_transport_key.public_key())
        );

        // (b) an entry signed by the device's own key projects VERIFIED in
        // both channels.
        let eve = Member::human("Eve's Laptop", "eve@example.com");
        let ledger = host.ledger_for(repo.path()).await.expect("ledger");
        let verified_a =
            sign_and_append(&ledger, chan_a, &eve, &device_signing_key, Timestamp::now()).await;
        let verified_b =
            sign_and_append(&ledger, chan_b, &eve, &device_signing_key, Timestamp::now()).await;
        let (_, view_a, _) = project_fresh(&host, "chan-a")
            .await
            .expect("chan-a projects");
        let (_, view_b, _) = project_fresh(&host, "chan-b")
            .await
            .expect("chan-b projects");
        for (view, id) in [(&view_a, verified_a), (&view_b, verified_b)] {
            assert!(!view.unrecognized.contains(&id), "{id} recognized");
            assert!(!view.unverified.contains(&id), "{id} verified");
        }

        // (c) the founder's own keys.toml holds no key for the enrolled
        // email — it was minted on the device's home, never here.
        assert!(
            !crate::keys::has_signing_key(home.path(), "eve@example.com")
                .expect("read the founder's keys.toml"),
            "the founder's machine must never have minted or received eve's key"
        );

        // (d) keys.json shows the enrolled device with the right
        // fingerprints, on both channels — and its `granted_by` handle is
        // exactly the grant id the retire step below targets.
        let signing_fp = crate::identity::fingerprint(&device_signing_key.public_key());
        let transport_fp = crate::identity::fingerprint(&device_transport_key.public_key());
        for (channel, grant) in [("chan-a", &grant_a), ("chan-b", &grant_b)] {
            let keys_response = keys_json(State(host.clone()), Path(channel.into())).await;
            assert_eq!(keys_response.status(), StatusCode::OK);
            let json: serde_json::Value =
                serde_json::from_str(&body_text(keys_response).await).expect("valid json");
            let eve_member = json["members"]
                .as_array()
                .expect("members array")
                .iter()
                .find(|m| m["email"] == "eve@example.com")
                .unwrap_or_else(|| panic!("eve is on {channel}'s roster"));
            let devices = eve_member["devices"].as_array().expect("devices array");
            assert_eq!(devices.len(), 1, "{devices:?}");
            assert_eq!(devices[0]["fingerprint"], signing_fp);
            assert_eq!(devices[0]["transport_fingerprint"], transport_fp);
            assert_eq!(devices[0]["granted_by"], grant.granted_by.to_string());
        }

        // --- `POST /channels/chan-a/keys/{grant}/retire` on ONE grant —
        // the other channel's grant must be unaffected.
        let retire_response = retire_device(
            State(host.clone()),
            Path(("chan-a".to_string(), grant_a.granted_by.to_string())),
            Form(RationaleForm {
                rationale: "eve's laptop was lost".to_string(),
            }),
        )
        .await;
        assert_eq!(retire_response.status(), StatusCode::OK);

        let (_, view_a, _) = project_fresh(&host, "chan-a")
            .await
            .expect("chan-a projects");
        assert!(
            view_a
                .keyring
                .get("eve@example.com")
                .expect("eve still has a grant on chan-a")
                .iter()
                .all(|g| g.retired_at.is_some()),
            "chan-a's grant is retired"
        );

        // The retired channel's grant actually stops something: a fresh
        // entry signed by the same device key, timestamped well after the
        // retirement, is unrecognized there — proving `retired_at` is a
        // real cutoff the fold consults, not just a recorded flag left
        // unconsulted by `KeyGrant::active_at` (finding 2, review round 1).
        let retire_cutoff = view_a
            .keyring
            .get("eve@example.com")
            .expect("eve's grant on chan-a")
            .iter()
            .filter_map(|g| g.retired_at)
            .max()
            .expect("the retire above set retired_at");
        let after_retire_ts = Timestamp::from_millis(retire_cutoff.as_millis() + 60_000);
        let unrecognized_a =
            sign_and_append(&ledger, chan_a, &eve, &device_signing_key, after_retire_ts).await;
        let (_, view_a, _) = project_fresh(&host, "chan-a")
            .await
            .expect("chan-a projects");
        assert!(
            view_a.unrecognized.contains(&unrecognized_a),
            "an entry stamped after chan-a's own retirement is unrecognized there"
        );

        // The other channel's grant still verifies a freshly signed entry.
        let still_verified_b =
            sign_and_append(&ledger, chan_b, &eve, &device_signing_key, Timestamp::now()).await;
        let (_, view_b, _) = project_fresh(&host, "chan-b")
            .await
            .expect("chan-b projects");
        assert!(!view_b.unrecognized.contains(&still_verified_b));
        assert!(!view_b.unverified.contains(&still_verified_b));
        assert!(
            view_b
                .keyring
                .get("eve@example.com")
                .expect("eve still has a grant on chan-b")
                .iter()
                .any(|g| g.retired_at.is_none()),
            "chan-b's grant was never touched by chan-a's retirement"
        );

        // --- `POST /channels/chan-b/members/eve@example.com/revoke` — a
        // later entry from the same device unrecognizes; an earlier one
        // keeps the standing it already had.
        // Stamped explicitly a minute *before* "now", not `Timestamp::now()`
        // itself (finding 3, review round 1): the assertion below needs
        // `earlier` to precede the revoke's own cutoff, and a backward
        // wall-clock step (an NTP correction, a resumed VM) between this
        // line and the revoke call just below could otherwise put
        // `earlier` after that cutoff, failing the test for a reason
        // unrelated to the code under test.
        let earlier_ts = Timestamp::from_millis(Timestamp::now().as_millis() - 60_000);
        let earlier = sign_and_append(&ledger, chan_b, &eve, &device_signing_key, earlier_ts).await;
        let revoke_response = revoke_member(
            State(host.clone()),
            Path(("chan-b".to_string(), "eve@example.com".to_string())),
            Form(RationaleForm {
                rationale: "eve is leaving".to_string(),
            }),
        )
        .await;
        assert_eq!(revoke_response.status(), StatusCode::OK);

        let (_, view_b, _) = project_fresh(&host, "chan-b")
            .await
            .expect("chan-b projects");
        let cutoff = view_b
            .keyring
            .get("eve@example.com")
            .expect("eve's grants")
            .iter()
            .filter_map(|g| g.retired_at)
            .max()
            .expect("revoke retired eve's chan-b grant");
        // A full minute past the retirement, so this can never tie the
        // cutoff regardless of how coarse the system clock is — the
        // fold's `>` comparison is what is under test, not real time.
        let later_ts = Timestamp::from_millis(cutoff.as_millis() + 60_000);
        let later = sign_and_append(&ledger, chan_b, &eve, &device_signing_key, later_ts).await;

        let (_, view_b, _) = project_fresh(&host, "chan-b")
            .await
            .expect("chan-b projects");
        assert!(
            !view_b.unrecognized.contains(&earlier),
            "the earlier entry, stamped before the revoke, stays recognized"
        );
        assert_eq!(
            view_b.standings.get(&earlier),
            Some(&Standing::Provisional),
            "the earlier entry keeps the standing it already had"
        );
        assert!(
            view_b.unrecognized.contains(&later),
            "a later entry from the revoked device is unrecognized"
        );
        assert_eq!(
            view_b.standings.get(&later),
            None,
            "an unrecognized entry carries no standing at all"
        );
    }
}

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
use serde::Deserialize;

use crate::host::{Host, Resolution};
use crate::render;

/// The web routes, to be merged into the host's router.
pub fn router(host: Arc<Host>) -> Router {
    Router::new()
        .route("/", get(index_page))
        .route("/new", get(new_page))
        .route("/settings", get(settings_page))
        .route("/channels", post(open_channel))
        .route("/repos", post(setup_repo))
        .route("/channels/{channel}", get(channel_page))
        .route("/channels/{channel}/sessions", post(launch_session))
        .route(
            "/channels/{channel}/sessions/{session}/steer",
            post(steer_session),
        )
        .route(
            "/channels/{channel}/sessions/{session}/stream",
            get(stream_session),
        )
        .route(
            "/channels/{channel}/artifacts/{artifact}",
            get(view_artifact),
        )
        .route("/channels/{channel}/rename", post(rename_channel))
        .route("/channels/{channel}/close", post(close_channel))
        .route("/channels/{channel}/reopen", post(reopen_channel))
        .route("/channels/{channel}/brief", get(channel_brief))
        .route("/channels/{channel}/entries/{entry}/{act}", post(verify))
        .with_state(host)
}

/// Resolve and project a channel reference, or surface the failure as an
/// appropriate HTTP status. Also yields the home substrate path — the
/// channel page's contextual open-an-inquiry form prefills it.
async fn project(
    host: &Host,
    channel: &str,
) -> Result<(ChannelId, ChannelView, std::path::PathBuf), Response> {
    let resolution = host
        .resolve(channel)
        .await
        .map_err(|err| internal(format!("resolving '{channel}': {err}")))?;
    let (ledger, id, substrate) = match resolution {
        Resolution::Resolved {
            ledger,
            id,
            substrate,
        } => (ledger, id, substrate),
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
    Ok((id, view, substrate))
}

fn internal(message: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
}

async fn index_page(State(host): State<Arc<Host>>) -> Response {
    // One projection sweep yields both the board and the cards — projection
    // is the page's whole cost, so it must not run twice.
    match host.overview().await {
        Ok((mut summaries, attention)) => {
            // Most recently active first — the resumption order.
            summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
            Html(render::index_html(&summaries, &attention)).into_response()
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
    /// Which harness runs it (`claude`, `opencode`); empty/unknown → default
    /// (`docs/adr/0024`).
    #[serde(default)]
    harness: String,
}

/// Launch an Agent Session from the channel page: resolve the workspace
/// (remembering a newly typed one), check the harness member is in the
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
    // The session's author is the harness member (docs/adr/0020); its entries
    // only project once it is in the Party (docs/adr/0017). Rather than reject
    // a launch in a fresh channel, bring the harness in: if the human at the
    // keyboard founded this channel, auto-grant membership (a founder-authored
    // MemberAdded) so the agent joins and starts work in one motion — the
    // grant is recorded, not hidden. A non-founder can't grant: they get
    // add_member's error naming who can (docs/adr/0017).
    // One agent per channel (for now): if an agent already serves this channel
    // (is in the Party), reuse it — the picker only chooses the agent the first
    // time. Otherwise the form's selection becomes the channel's agent, granted
    // below.
    let harness = match crate::launch::channel_harness(&view.party) {
        Some(established) => established,
        None => crate::launch::harness_by_id(form.harness.trim()),
    };
    let harness_member = harness.member();
    let harness_is_member = view.party.iter().any(|m| m.email == harness_member.email);
    if !view.party.is_empty() && !harness_is_member {
        let granter = match crate::host::git_user(&substrate) {
            Ok(granter) => granter,
            Err(err) => {
                return internal(format!(
                    "no author identity: {err} (set git config user.name / user.email)"
                ));
            }
        };
        if let Err(err) = host.add_member(&channel, &granter, harness_member).await {
            return (StatusCode::FORBIDDEN, format!("{err:#}")).into_response();
        }
    }
    let junto_home = match crate::host::junto_home() {
        Ok(home) => home,
        Err(err) => return internal(format!("no junto home: {err}")),
    };
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
    match crate::launch::launch(
        host.clone(),
        id,
        channel.clone(),
        workspace,
        intent,
        harness,
    )
    .await
    {
        Ok(_session) => Redirect::to(&format!("/channels/{id}")).into_response(),
        Err(err) => internal(format!("launch failed: {err:#}")),
    }
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
    match crate::launch::steer(
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
    let Ok(artifact_id) = artifact.parse::<EntryId>() else {
        return (StatusCode::BAD_REQUEST, "not an artifact id").into_response();
    };
    let (_id, view, _substrate) = match project(&host, &channel).await {
        Ok(projected) => projected,
        Err(response) => return response,
    };
    let Some(entry) = view.entries.iter().find(|e| e.id == artifact_id) else {
        return (StatusCode::NOT_FOUND, "no such artifact in this channel").into_response();
    };
    let EntryPayload::ArtifactAttached {
        kind, provenance, ..
    } = &entry.payload
    else {
        return (StatusCode::BAD_REQUEST, "that entry is not an artifact").into_response();
    };
    let Some(file) = provenance.first() else {
        return (StatusCode::NOT_FOUND, "artifact has no stored content").into_response();
    };
    let Some(path) = file_uri_to_path(file.uri.as_str()) else {
        return (StatusCode::BAD_REQUEST, "artifact is not a local file").into_response();
    };
    // Defense in depth: only ever read under this machine's artifacts root.
    let artifacts_root = match crate::host::junto_home() {
        Ok(home) => home.join("artifacts"),
        Err(err) => return internal(format!("no junto home: {err}")),
    };
    let (canon_path, canon_root) = match (
        dunce::canonicalize(&path),
        dunce::canonicalize(&artifacts_root),
    ) {
        (Ok(p), Ok(root)) => (p, root),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                "artifact content is not on this machine",
            )
                .into_response();
        }
    };
    if !canon_path.starts_with(&canon_root) {
        return (StatusCode::FORBIDDEN, "artifact path is outside the store").into_response();
    }
    let content = match std::fs::read_to_string(&canon_path) {
        Ok(content) => content,
        Err(err) => {
            return (StatusCode::NOT_FOUND, format!("reading artifact: {err}")).into_response();
        }
    };
    // The card's `<details>` lazy-loads this inline. A memo is the agent's
    // prose (rendered as sanitized CommonMark); a diff gets per-line colour;
    // everything else stays verbatim as text.
    let html =
        |body: String| ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    match render::artifact_format(kind) {
        render::ArtifactFormat::Markdown => html(render::render_markdown(&content)),
        render::ArtifactFormat::Diff => html(render::render_diff(&content)),
        render::ArtifactFormat::Raw => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            content,
        )
            .into_response(),
    }
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
    let entry = LedgerEntry {
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
    let entry = LedgerEntry {
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload,
    };
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

    let entry = LedgerEntry {
        id: EntryId::new(),
        channel: id,
        author,
        timestamp: Timestamp::now(),
        payload,
    };
    if let Err(err) = guard.append(entry).await {
        return internal(format!("append failed: {err}"));
    }
    drop(guard);

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

async fn channel_brief(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view, _substrate)) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{ApprovalRequirement, GateStatus, Member, Standing};
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

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
        )
        .await
        .expect("add bot");
        let target = EntryId::new();
        let ledger = host.ledger_for(dir.path()).await.expect("ledger");
        ledger
            .lock()
            .await
            .append(LedgerEntry {
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
        }
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
            .add_member("web-test", &founder, crate::launch::harness_member())
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
                harness: String::new(),
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
                harness: String::new(),
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
            entries: vec![entry.clone()],
            party: Vec::new(),
            unrecognized: Default::default(),
            sessions: Default::default(),
            closed: false,
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
}

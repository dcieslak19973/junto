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
//! And one POST — the **human write surface**: verification acts (ratify /
//! park / approve / reject) submitted from the channel page's forms, so the
//! human's slow loop happens on the one surface instead of through an agent
//! relaying it. Authored as the machine user's git identity
//! ([`crate::host::git_user`]) — identity stays claimed (`docs/adr/0012`),
//! this is a default, not an identity system. Verification acts only:
//! recording assertions and proposing actions remain agent (MCP) territory.
//! Each web write triggers a **best-effort background sync** with `origin` —
//! the page is a terminal-less human's only affordance, so the durable record
//! must not wait for an agent to run `sync_channel`.

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
        .route("/channels/{channel}", get(channel_page))
        .route("/channels/{channel}/brief", get(channel_brief))
        .route("/channels/{channel}/entries/{entry}/{act}", post(verify))
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

async fn channel_page(State(host): State<Arc<Host>>, Path(channel): Path<String>) -> Response {
    match project(&host, &channel).await {
        Ok((id, view)) => {
            let name = view.name.clone().unwrap_or_else(|| channel.clone());
            // The sidebar: every channel, most recently active first —
            // best-effort, an empty nav never blocks the page itself.
            let mut nav = host.inventory().await.unwrap_or_default();
            nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
            Html(render::channel_html(&nav, &name, &id, &view)).into_response()
        }
        Err(response) => response,
    }
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
        };
        let html = crate::render::channel_html(&[], "web-test", &channel, &view);
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

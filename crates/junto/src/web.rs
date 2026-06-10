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
//! Web writes ride to the remote on the next `sync_channel`.

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

/// The form body of a verification act: the rationale and the author's member
/// code (`docs/adr/0017`) — the act and target come from the URL, the author
/// from git config.
#[derive(Debug, Deserialize)]
struct ActForm {
    rationale: String,
    /// The member code of the machine user (required once the channel has a
    /// Party; blank tolerated for pre-genesis channels).
    #[serde(default)]
    code: String,
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
            "ratify" => EntryPayload::Ratification { target, rationale },
            _ => EntryPayload::Park { target, rationale },
        },
        "approve" | "reject" if view.gate_status(&target).is_some() => match act.as_str() {
            "approve" => EntryPayload::Approval { target, rationale },
            _ => EntryPayload::Rejection { target, rationale },
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

    // The write-surface guardrail (docs/adr/0017): the author must be in the
    // channel's Party and present their member code.
    let code = form.code.trim();
    let code = (!code.is_empty()).then_some(code);
    if let Err(err) = host.authorize_write(&view, &author, code) {
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
    // Back to the page, id-addressed (ids are URL-safe; names may not be).
    Redirect::to(&format!("/channels/{id}")).into_response()
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
    /// dir, a channel founded by the repo's git user (the web-write author,
    /// `docs/adr/0017`), and one entry by a granted "Bot" member.
    struct WebFixture {
        _dirs: Vec<TempDir>,
        host: Arc<Host>,
        channel: ChannelId,
        target: EntryId,
        /// The founder's (= git user's) member code, for the act forms.
        code: String,
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
        let code = opened.founder_code.code;
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
            code,
        }
    }

    fn assertion() -> EntryPayload {
        EntryPayload::Assertion {
            statement: "claim".into(),
            rationale: "because".into(),
            provenance: vec![],
        }
    }

    fn proposal() -> EntryPayload {
        EntryPayload::Proposal {
            action: "merge it".into(),
            rationale: "ready".into(),
            provenance: vec![],
            requirement: ApprovalRequirement::Count(1),
        }
    }

    async fn post_act(
        host: Arc<Host>,
        channel: String,
        entry: String,
        act: &str,
        rationale: &str,
        code: &str,
    ) -> Response {
        verify(
            State(host),
            Path((channel, entry, act.to_string())),
            Form(ActForm {
                rationale: rationale.into(),
                code: code.into(),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn web_ratify_moves_standing_with_git_author() {
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked it",
            &fx.code,
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
            &fx.code,
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
            &fx.code,
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
            &fx.code,
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
            &fx.code,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_or_missing_member_codes_are_forbidden() {
        // The guardrail of docs/adr/0017 on the web surface: the git-user
        // author must present their member code.
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "r",
            "WRONG0",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "r",
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // Nothing was appended: the assertion is still provisional.
        let Resolution::Resolved { ledger, id, .. } =
            fx.host.resolve(&fx.channel.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(matches!(
            view.standing(&fx.target),
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
        };
        let html = crate::render::channel_html("web-test", &channel, &view);
        assert!(html.contains(&format!("/channels/{channel}/entries/{}/ratify", entry.id)));
        assert!(html.contains("name=\"rationale\""));
    }
}

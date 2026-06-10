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

/// The form body of a verification act: the rationale, nothing else (the act
/// and target come from the URL, the author from git config).
#[derive(Debug, Deserialize)]
struct ActForm {
    rationale: String,
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

    /// A host over one fresh git repo with a configured user, plus an opened
    /// channel holding one entry built by `payload`.
    async fn host_with_entry(payload: EntryPayload) -> (TempDir, Arc<Host>, ChannelId, EntryId) {
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
        let host = Host::fixed(vec![dir.path().to_path_buf()]);
        let channel = host
            .open_channel(
                None,
                "web-test",
                Member::human("Opener", "o@example.com"),
                None,
            )
            .await
            .expect("open channel");
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
        (dir, host, channel, target)
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
    ) -> Response {
        verify(
            State(host),
            Path((channel, entry, act.to_string())),
            Form(ActForm {
                rationale: rationale.into(),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn web_ratify_moves_standing_with_git_author() {
        let (_dir, host, channel, target) = host_with_entry(assertion()).await;
        let response = post_act(
            host.clone(),
            "web-test".into(),
            target.to_string(),
            "ratify",
            "checked it",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let resolution = host.resolve(&channel.to_string()).await.unwrap();
        let Resolution::Resolved { ledger, id, .. } = resolution else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(matches!(view.standing(&target), Some(Standing::Ratified)));
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
        let (_dir, host, channel, target) = host_with_entry(proposal()).await;
        let response = post_act(
            host.clone(),
            channel.to_string(),
            target.to_string(),
            "approve",
            "lgtm",
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let Resolution::Resolved { ledger, id, .. } =
            host.resolve(&channel.to_string()).await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert!(matches!(
            view.gate_status(&target),
            Some(GateStatus::Approved)
        ));
    }

    #[tokio::test]
    async fn empty_rationale_is_refused() {
        let (_dir, host, _channel, target) = host_with_entry(assertion()).await;
        let response = post_act(host, "web-test".into(), target.to_string(), "ratify", "  ").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn cross_kind_acts_are_refused() {
        // Approving an assertion (it's not a proposal) is a 400, not a
        // silently-dangling act.
        let (_dir, host, _channel, target) = host_with_entry(assertion()).await;
        let response = post_act(host, "web-test".into(), target.to_string(), "approve", "r").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_acts_are_404() {
        let (_dir, host, _channel, target) = host_with_entry(assertion()).await;
        let response = post_act(host, "web-test".into(), target.to_string(), "yolo", "r").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
        };
        let html = crate::render::channel_html("web-test", &channel, &view);
        assert!(html.contains(&format!("/channels/{channel}/entries/{}/ratify", entry.id)));
        assert!(html.contains("name=\"rationale\""));
    }
}

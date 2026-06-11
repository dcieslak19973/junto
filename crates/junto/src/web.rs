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

/// The form body of a verification act: the rationale and the author's member
/// code (`docs/adr/0017`) — the act and target come from the URL, the author
/// from git config.
#[derive(Debug, Deserialize)]
struct ActForm {
    rationale: String,
    /// The member code of the machine user (required once the channel has a
    /// Party; blank falls back to the code remembered in the cookie).
    #[serde(default)]
    code: String,
    /// Where to return after acting (the focus board sets "/"); only local
    /// paths are honored.
    #[serde(default)]
    back: String,
}

/// The cookie that remembers the member code after a successful act, so a
/// batch of verifications is N clicks, not N pastes (`docs/attention.md`).
/// HttpOnly, SameSite=Strict, localhost-only host — the same machine-local
/// accident-proofing posture as the code store itself (`docs/adr/0017`).
const CODE_COOKIE: &str = "junto_member_code";

/// The remembered member code from the request's cookies, if any.
fn remembered_code(headers: &axum::http::HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == CODE_COOKIE).then(|| value.to_string())
    })
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
    headers: axum::http::HeaderMap,
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
    // The rationale is cloned into the payload: the original is still needed
    // for the retry page if the member-code check refuses the act below.
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

    // The write-surface guardrail (docs/adr/0017): the author must be in the
    // channel's Party and present their member code — typed, or remembered
    // from the last successful act (docs/attention.md: batch verification is
    // N clicks, not N pastes).
    let typed = form.code.trim().to_string();
    let remembered = remembered_code(&headers);
    let code = if typed.is_empty() {
        remembered.clone()
    } else {
        Some(typed.clone())
    };
    if let Err(err) = host.authorize_write(&view, &author, code.as_deref()) {
        // A code mishap must not cost the human their work: keep the typed
        // rationale, explain the refusal in place, and ask only for the code.
        // If the failing code came from the remember-cookie, the human typed
        // nothing wrong — forget the stale cookie so retyping takes.
        drop(guard); // inventory() re-locks the ledgers below
        let cookie_was_stale = typed.is_empty() && remembered.is_some();
        let subject = view
            .entries
            .iter()
            .find(|e| e.id == target)
            .and_then(|entry| match &entry.payload {
                EntryPayload::Assertion { statement, .. } => Some(statement.clone()),
                EntryPayload::Proposal { action, .. } => Some(action.clone()),
                _ => None,
            });
        let mut nav = host.inventory().await.unwrap_or_default();
        nav.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
        let back = safe_back(&form.back)
            .map(str::to_string)
            .unwrap_or_else(|| format!("/channels/{id}"));
        let page = Html(render::act_retry_html(
            &nav,
            &render::ActRetry {
                channel: &id,
                name: view.name.as_deref().unwrap_or(&channel),
                entry: target,
                act: &act,
                rationale: &rationale,
                back: &back,
                message: &format!("{err:#}"),
                subject: subject.as_deref(),
                cookie_forgotten: cookie_was_stale,
            },
        ));
        if cookie_was_stale {
            let clear = format!("{CODE_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
            return (StatusCode::FORBIDDEN, [(header::SET_COOKIE, clear)], page).into_response();
        }
        return (StatusCode::FORBIDDEN, page).into_response();
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
    // Remember a freshly typed, just-validated code for the next act.
    if !typed.is_empty() && remembered.as_deref() != Some(typed.as_str()) {
        let cookie =
            format!("{CODE_COOKIE}={typed}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000");
        return ([(header::SET_COOKIE, cookie)], Redirect::to(&destination)).into_response();
    }
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
        code: &str,
    ) -> Response {
        post_act_with(host, channel, entry, act, rationale, code, "", None).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn post_act_with(
        host: Arc<Host>,
        channel: String,
        entry: String,
        act: &str,
        rationale: &str,
        code: &str,
        back: &str,
        cookie: Option<&str>,
    ) -> Response {
        let mut headers = axum::http::HeaderMap::new();
        if let Some(cookie) = cookie {
            headers.insert(
                header::COOKIE,
                format!("{CODE_COOKIE}={cookie}").parse().unwrap(),
            );
        }
        verify(
            State(host),
            Path((channel, entry, act.to_string())),
            headers,
            Form(ActForm {
                rationale: rationale.into(),
                code: code.into(),
                back: back.into(),
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
    async fn remembered_code_and_back_path() {
        // First act: typed code + back to the board — the response carries
        // both the redirect to "/" and the remember-cookie.
        let fx = host_with_entry(assertion()).await;
        let response = post_act_with(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked",
            &fx.code,
            "/",
            None,
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
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("remember-cookie set");
        assert!(cookie.contains(&format!("{CODE_COOKIE}={}", fx.code)));
        assert!(cookie.contains("HttpOnly"));

        // Second act: blank code, cookie supplies it; a hostile back path is
        // ignored in favor of the channel page.
        let fx2 = host_with_entry(proposal()).await;
        let response = post_act_with(
            fx2.host.clone(),
            "web-test".into(),
            fx2.target.to_string(),
            "approve",
            "lgtm",
            "",
            "//evil.example.com",
            Some(&fx2.code),
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
            &fx.code,
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
    async fn wrong_or_missing_member_codes_are_forbidden() {
        // The guardrail of docs/adr/0017 on the web surface: the git-user
        // author must present their member code. The refusal is a retry page
        // that keeps the typed rationale — a wrong code costs one retyped
        // code, never a retyped why.
        let fx = host_with_entry(assertion()).await;
        let response = post_act(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked the diff carefully",
            "WRONG0",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let page = body_text(response).await;
        assert!(
            page.contains("checked the diff carefully"),
            "the typed rationale survives the refusal"
        );
        assert!(
            page.contains(&format!("/entries/{}/ratify", fx.target)),
            "the page re-offers the same act"
        );
        assert!(
            page.contains("web@example.com"),
            "the refusal names whose code is expected"
        );

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

    #[tokio::test]
    async fn stale_remembered_code_is_forgotten() {
        // A wrong code that came from the remember-cookie is the cookie's
        // fault, not the human's: the refusal clears the cookie and says so,
        // so the next typed code actually takes.
        let fx = host_with_entry(assertion()).await;
        let response = post_act_with(
            fx.host.clone(),
            "web-test".into(),
            fx.target.to_string(),
            "ratify",
            "checked",
            "", // nothing typed —
            "/",
            Some("STALE0"), // — the cookie supplied the (wrong) code
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let clear = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("clearing cookie set")
            .to_string();
        assert!(clear.contains("Max-Age=0"), "cookie cleared: {clear}");
        let page = body_text(response).await;
        assert!(page.contains("forgotten"), "the page explains the clearing");
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

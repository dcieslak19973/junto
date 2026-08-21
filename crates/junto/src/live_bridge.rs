//! The annotation → steer bridge: hands validated watcher annotations to the
//! driving agent as steering context.
//!
//! Two delivery paths, split on [`Annotation::urgent`]:
//!
//! - **Urgent** annotations interrupt in place: [`deliver`] re-anchors each
//!   one against the session's current worktree (best-effort — see
//!   [`reanchor_batch`]), renders the batch with [`format_steer`], and pushes
//!   it straight at the running turn's control channel via
//!   [`crate::launch::steer_live`]. If no turn is currently running
//!   ([`crate::launch::NotLive`]), the batch falls back onto
//!   [`crate::live_plane::LivePlane`]'s pending queue rather than being
//!   dropped — a watcher's urgent comment must reach the agent *somehow*,
//!   even if not this instant.
//! - **Ordinary** annotations just join the pending queue. They are flushed
//!   as one batch at the next turn boundary by [`flush_pending`], wired
//!   directly into `crate::launch`'s `LiveSessions::begin` so no future
//!   caller can add a `begin` site that forgets it.
//!
//! Both paths converge on [`deliver_batch`], which renders and calls
//! `steer_live`, falling back to the pending queue on `NotLive` either way:
//! nothing this module touches may ever be silently dropped. The queue
//! itself lives on `LivePlane` (keyed by session), not on the per-turn
//! `SessionLive` — see that struct's `pending` field doc for why: a queue
//! tied to `SessionLive`'s lifetime cannot survive the turn boundary it
//! exists to cross.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use junto_kernel::{Anchor, Annotation, EntryId};
use junto_substrate_git::reanchor::{Reanchor, reanchor};

use crate::host::{Host, Resolution};
use crate::launch::NotLive;

/// Render a batch of annotations as one steer message for the driving
/// agent's context.
///
/// `reanchored[i]` is the current position of `annotations[i]`'s anchor
/// (`None` when re-anchoring wasn't attempted or failed — see
/// [`reanchor_batch`]); the two slices are parallel by index, matching how
/// [`deliver_batch`] builds them together.
///
/// The agent has no other view of the watcher's screen, so each block names
/// **who** commented, **where** (the pinned `path:span`, plus where that
/// span sits now if it moved), quotes the **pinned excerpt** if the
/// annotation carried one — it travels with the comment, every line of it
/// prefixed so a multi-line span (the normal case: a `CodeAnchor` pins a
/// line *range*) reads as one quoted block rather than bleeding into the
/// comment that follows — because the current file content may no longer
/// match what the watcher was actually looking at. An `Orphaned` re-anchor
/// says so plainly rather than pointing at a line range that no longer
/// corresponds to anything the watcher commented on.
pub(crate) fn format_steer(annotations: &[Annotation], reanchored: &[Option<Reanchor>]) -> String {
    let mut out = String::new();
    for (i, annotation) in annotations.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let current = reanchored.get(i).copied().flatten();
        let location = match &annotation.anchor {
            Anchor::Code(code) => {
                let pinned = format!("{}:{}-{}", code.path, code.span.start, code.span.end);
                match current {
                    None | Some(Reanchor::Exact { .. }) => pinned,
                    Some(Reanchor::Moved { span }) => {
                        format!("{pinned} (moved to {}-{})", span.start, span.end)
                    }
                    Some(Reanchor::Orphaned) => format!(
                        "{pinned} (code has changed since — excerpt shows the commented version)"
                    ),
                }
            }
            // LiveDoc op ids are stable by construction (CRDT positions
            // don't drift the way line numbers do), so a stream anchor has
            // no re-anchored position to report — nothing to compute here.
            Anchor::Stream(stream) => format!("on conversation event {}", stream.op_id),
        };
        let on = if matches!(annotation.anchor, Anchor::Stream(_)) {
            location
        } else {
            format!("on {location}")
        };
        out.push_str(&format!(
            "[watcher comment — {} {on}]\n",
            annotation.author.email
        ));
        if let Some(excerpt) = &annotation.excerpt {
            // Every line quoted, not just the first: a `CodeAnchor`'s span
            // is a line *range* (the excerpt is normally multi-line), and
            // an unmarked line reads to the model as the start of the
            // comment body rather than still-quoted code.
            for line in excerpt.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&annotation.body);
        out.push('\n');
    }
    out
}

/// Re-anchor each of `annotations`' [`Anchor::Code`] anchors against
/// `workspace`, one [`Reanchor`] outcome per annotation (parallel to
/// `annotations` by index, `None` for a [`Anchor::Stream`] anchor or when
/// `workspace` is unknown).
///
/// Best-effort: any error from `reanchor` (not a git repo, unknown commit,
/// `git` unavailable, …) becomes `None` for that annotation, which
/// [`format_steer`] renders as pinned-only — a git hiccup must never cost
/// the watcher their comment, only the "where is it now" enrichment.
async fn reanchor_batch(
    workspace: Option<&Path>,
    annotations: &[Annotation],
) -> Vec<Option<Reanchor>> {
    let mut out = Vec::with_capacity(annotations.len());
    for annotation in annotations {
        let outcome = match (workspace, &annotation.anchor) {
            (Some(worktree), Anchor::Code(code)) => reanchor(worktree, code).await.ok(),
            _ => None,
        };
        out.push(outcome);
    }
    out
}

/// Render `annotations` and steer the running turn with them, requeuing the
/// whole batch onto [`crate::live_plane::LivePlane`]'s pending queue if
/// delivery can't land right now — a channel that fails to resolve, or a
/// `steer_live` call that reports [`NotLive`] (no turn currently running).
/// Shared by [`deliver`]'s urgent path and [`flush_pending`]'s boundary
/// flush — the only difference between the two callers is *when* this runs,
/// not what it does.
///
/// Deliberately does **not** require a live [`crate::live_plane::SessionLive`]
/// to proceed or to requeue: the pending queue lives on `LivePlane` itself,
/// independent of whether a `SessionLive` is currently registered for
/// `session` (it may have ended between validation and this call, or the
/// turn this batch was meant for may already have finished within the
/// flush delay) — there is always somewhere to put the batch back.
async fn deliver_batch(
    host: Arc<Host>,
    channel: String,
    session: EntryId,
    annotations: Vec<Annotation>,
) {
    // An empty batch is a no-op, not an error — the private helper in the
    // one module whose contract is that nothing here may panic the session
    // guards its own invariant rather than trusting every call site to.
    let Some(first) = annotations.first() else {
        return;
    };
    // Resolved the same way `web.rs`'s `steer_session` handler resolves its
    // `channel` path param before calling `steer_live`.
    let Ok(Resolution::Resolved { id, .. }) = host.resolve(&channel).await else {
        host.live_plane().queue_pending(session, annotations);
        return;
    };
    // Best-effort: `None` (no `SessionLive`, or no workspace recorded on it
    // yet) just means `reanchor_batch` renders every anchor pinned-only —
    // never a reason to withhold the comment itself.
    let workspace = host.live_plane().get(session).and_then(|live| {
        live.workspace
            .lock()
            .expect("live plane workspace lock")
            .clone()
    });
    let reanchored = reanchor_batch(workspace.as_deref(), &annotations).await;
    let message = format_steer(&annotations, &reanchored);
    // The batch becomes one steer message, so the ledger's steer note
    // records a single author: the first annotation's, in batch order.
    let steered_by = first.author.clone();
    if let Err(NotLive) =
        crate::launch::steer_live(Arc::clone(&host), id, channel, session, steered_by, message)
            .await
    {
        host.live_plane().queue_pending(session, annotations);
    }
}

/// Hand newly validated watcher `annotations` for `session` (in `channel`) to
/// the steering bridge.
///
/// Splits the batch on [`Annotation::urgent`]. Urgent annotations are
/// attempted (and, on failure, requeued) via [`deliver_batch`] **first**,
/// before ordinary ones join the pending queue — so a requeued urgent batch
/// keeps priority ahead of same-frame ordinary annotations in the next
/// flush, rather than an ordinary annotation queued first pushing an urgent
/// one to the back. Fire-and-forget, like every other live-plane tap
/// (`crate::live_plane` module docs: the never-block invariant).
pub(crate) async fn deliver(
    host: Arc<Host>,
    channel: String,
    session: EntryId,
    annotations: Vec<Annotation>,
) {
    let (urgent, ordinary): (Vec<Annotation>, Vec<Annotation>) =
        annotations.into_iter().partition(|a| a.urgent);
    if !urgent.is_empty() {
        deliver_batch(Arc::clone(&host), channel.clone(), session, urgent).await;
    }
    if !ordinary.is_empty() {
        host.live_plane().queue_pending(session, ordinary);
    }
}

/// Check `session`'s pending queue at a turn boundary and, if it carries
/// anything, spawn a delayed delivery of the whole batch as one steer
/// message.
///
/// Called from inside `crate::launch`'s `LiveSessions::begin` itself (not
/// left to each call site): a queued annotation must survive exactly the
/// turn boundary `begin` represents, so the flush is part of what `begin`
/// *means*, not an extra step a future caller could add a `begin` site
/// without.
///
/// Only peeks (`LivePlane::has_pending`) before deciding whether to spawn —
/// the actual drain (`LivePlane::take_pending`) happens inside the spawned
/// task, *after* the delay, so a batch is never held outside the shared
/// queue (and thus unreachable if anything fails) while waiting out the
/// delay. The 2s delay itself lets the turn's control receiver — `begin`'s
/// own return value — actually get selected on by the fresh turn loop
/// before a steer lands on it, the same margin `steer_live` implicitly
/// relies on when called well after a turn is confirmed running.
///
/// **Known gap:** for a session `begin` opened non-steerable
/// (`LiveSessions::begin`'s `steerable` flag — currently only the Outcome
/// loop, `crate::launch::spawn_outcome_loop`) every flush this function
/// runs still calls `deliver_batch`, which still calls `steer_live`, which
/// still reports `NotLive` for a non-steerable session exactly as it would
/// for a truly idle one — so the batch is drained and immediately requeued,
/// every time, and never actually reaches the agent. A watcher's comment on
/// a running, un-steered Outcome-loop turn queues until a human steers that
/// session from the web UI (which resumes it over the *steerable*
/// `spawn_turn` path instead); inside a pure, never-steered loop it may
/// never be delivered. Accepted trade-off, not an oversight: the
/// alternative — a real control sender for a turn that never reads it —
/// makes `steer_live` fake-succeed and write a false ledger note, which is
/// worse than an honest queue that doesn't drain (Task 8 fix round finding 2).
pub(crate) fn flush_pending(host: Arc<Host>, channel: String, session: EntryId) {
    if !host.live_plane().has_pending(session) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let batch = host.live_plane().take_pending(session);
        if batch.is_empty() {
            return;
        }
        deliver_batch(host, channel, session, batch).await;
    });
}

#[cfg(test)]
mod tests {
    use junto_kernel::{
        Anchor, AnnotationId, CodeAnchor, CommitOid, ContentDigest, Member, Span, Timestamp,
    };

    use super::*;

    /// A `CodeAnchor` annotation with the given path/span/excerpt/body, for
    /// `format_steer`'s pure-function tests — no host, no git, no async.
    fn code_annotation(
        author_email: &str,
        path: &str,
        start: u32,
        end: u32,
        excerpt: Option<&str>,
        body: &str,
    ) -> Annotation {
        Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", author_email),
            anchor: Anchor::Code(CodeAnchor {
                commit: CommitOid::new("a".repeat(40)).unwrap(),
                path: path.into(),
                blob: ContentDigest::new("sha256:deadbeef").unwrap(),
                span: Span::new(start, end).unwrap(),
            }),
            body: body.into(),
            excerpt: excerpt.map(Into::into),
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        }
    }

    #[test]
    fn format_renders_anchor_excerpt_and_body() {
        let ann = code_annotation(
            "dan@x.com",
            "src/foo.rs",
            12,
            14,
            Some("let x = 1;"),
            "off by one",
        );
        let out = format_steer(
            &[ann],
            &[Some(Reanchor::Moved {
                span: Span::new(15, 17).unwrap(),
            })],
        );
        assert!(out.contains("dan@x.com"));
        assert!(out.contains("src/foo.rs:12-14"));
        assert!(out.contains("moved to 15-17"));
        assert!(out.contains("> let x = 1;"));
        assert!(out.contains("off by one"));
    }

    #[test]
    fn orphaned_anchor_is_stated_not_hidden() {
        let ann = code_annotation("dan@x.com", "src/foo.rs", 3, 3, Some("old line"), "why?");
        let out = format_steer(&[ann], &[Some(Reanchor::Orphaned)]);
        assert!(out.contains("code has changed since"));
    }

    #[test]
    fn exact_reanchor_renders_without_a_qualifier() {
        let ann = code_annotation("dan@x.com", "src/foo.rs", 5, 5, None, "still true");
        let out = format_steer(
            &[ann],
            &[Some(Reanchor::Exact {
                span: Span::new(5, 5).unwrap(),
            })],
        );
        assert!(out.contains("src/foo.rs:5-5"));
        assert!(!out.contains("moved to"));
        assert!(!out.contains("changed since"));
    }

    #[test]
    fn batch_concatenates_in_order() {
        let a = code_annotation("a@x.com", "a.rs", 1, 1, None, "first");
        let b = code_annotation("b@x.com", "b.rs", 2, 2, None, "second");
        let out = format_steer(&[a, b], &[None, None]);
        assert!(out.find("first").unwrap() < out.find("second").unwrap());
    }

    #[test]
    fn missing_excerpt_omits_the_quote_block() {
        let ann = code_annotation("a@x.com", "a.rs", 1, 1, None, "no excerpt here");
        let out = format_steer(&[ann], &[None]);
        assert!(!out.contains('>'));
    }

    #[test]
    fn multiline_excerpt_quotes_every_line() {
        let ann = code_annotation(
            "a@x.com",
            "a.rs",
            1,
            2,
            Some("let x = 1;\nlet y = 2;"),
            "both wrong",
        );
        let out = format_steer(&[ann], &[None]);
        assert!(
            out.contains("> let x = 1;\n> let y = 2;\n"),
            "every excerpt line must carry its own quote marker: {out}"
        );
    }

    #[test]
    fn stream_anchor_renders_conversation_event() {
        let ann = Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", "dan@x.com"),
            anchor: Anchor::Stream(junto_kernel::StreamAnchor {
                session: EntryId::new(),
                op_id: "12@7".into(),
            }),
            body: "what's this doing?".into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        };
        let out = format_steer(&[ann], &[None]);
        assert!(out.contains("on conversation event 12@7"));
    }

    /// A fresh test host: a temp git repo (git user `Dan <dan@x.com>`) and a
    /// channel opened by that user — enough for `host.resolve` to succeed in
    /// `deliver_batch`, with no `LiveSessions` feed registered at all, so
    /// `steer_live` reports `NotLive`. Returns the host, the channel's
    /// resolvable ref (its id, as a string), and the temp dirs — kept alive
    /// by the caller for the fixture's lifetime, same convention as
    /// `live_ws.rs`'s `fixture`.
    async fn host_with_channel() -> (Arc<Host>, String, tempfile::TempDir, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Dan"), ("user.email", "dan@x.com")] {
            assert!(
                std::process::Command::new("git")
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
        let founder = Member::human("Dan", "dan@x.com");
        let opened = host
            .open_channel(None, "live-bridge-test", founder, None)
            .await
            .expect("open channel");
        (host, opened.id.to_string(), dir, member_home)
    }

    #[tokio::test]
    async fn not_live_urgent_delivery_requeues_the_batch() {
        let (host, channel_ref, _dir, _member_home) = host_with_channel().await;
        let session = EntryId::new();
        // A `SessionLive` exists (so re-anchoring/workspace lookups have
        // somewhere to look) but no `LiveSessions` feed was ever begun for
        // it, so `steer_live` must report `NotLive`.
        host.live_plane().begin(session);

        let mut annotation = code_annotation("dan@x.com", "src/foo.rs", 1, 1, None, "please look");
        annotation.urgent = true;
        let id = annotation.id;

        deliver(Arc::clone(&host), channel_ref, session, vec![annotation]).await;

        let requeued = host.live_plane().take_pending(session);
        assert_eq!(
            requeued.len(),
            1,
            "the undelivered batch must land back on the pending queue"
        );
        assert_eq!(requeued[0].id, id);
    }
}

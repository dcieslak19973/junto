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

use junto_kernel::{Anchor, Annotation, EntryId, Member};
use junto_live::LiveDoc;
use junto_substrate_git::reanchor::{Reanchor, reanchor};

use crate::host::{Host, Resolution};
use crate::launch::NotLive;

/// Hard cap on an annotation's `body` bytes carried into the steer message
/// and, from there, verbatim into the DURABLE ledger: [`deliver_batch`] ->
/// `crate::launch::steer_live` -> `record_steer_note` -> `append` writes
/// `SessionUpdated { note: format!("steer: {message}") }` to
/// `refs/junto/*`. Nothing in `validate_annotation_update` bounds this
/// field (only the 8 MiB websocket frame limit does, and only per
/// *frame*, not per body). 4 KiB is generous for real prose (several long
/// paragraphs) while keeping one oversized or malicious comment's `body`
/// from dominating the record.
///
/// `body` is only ONE of the inputs [`format_steer`] renders into that
/// same durable string. This cap alone does NOT make the rendered message
/// bounded — that overclaim is what the final branch review's residual
/// caught (IMPORTANT 3): [`MAX_ANNOTATION_EXCERPT_BYTES`] bounds the
/// sibling excerpt fields, [`MAX_ANCHOR_LOCATION_BYTES`] bounds the anchor
/// fields named in the block header, and [`MAX_STEER_MESSAGE_BYTES`]
/// bounds the whole rendered batch. Only all four together bound every
/// remote-CHOSEN byte on this path — the one exception is
/// `annotation.author.email`, also rendered into the header: it carries no
/// cap of its own because it needs none. `validate_annotation_update`
/// requires it to equal `sender_email` — the authenticated connection's
/// own email, fixed once per connection at the handshake
/// (`live_ws::authenticate`, checked against `ChannelView::party`) and
/// never re-derived per annotation — and that email was itself set once,
/// by the founder-authored `MemberAdded`/`ChannelOpened` entry that
/// admitted the member (`ChannelView::party` and `ChannelView::keyring`
/// are each folded from those same entries, independently — Task 10
/// widened the keyring `authenticate` and this bridge's own signature
/// check read from `view.party`'s first grant to `view.keyring`'s every
/// ACTIVE one, but neither was ever "the party projection" itself). So
/// its length is fixed at member admission, not chosen per annotation the
/// way every capped field here is. Stating
/// that any one of the four caps alone keeps the rendered message finite
/// would be the same overclaim again.
const MAX_ANNOTATION_BODY_BYTES: usize = 4096;

/// Hard cap, independent of [`MAX_ANNOTATION_BODY_BYTES`], on any single
/// remote-supplied excerpt quoted into the steer message. [`format_steer`]
/// applies this uniformly to whichever excerpt source it ends up quoting:
/// `annotation.excerpt` (attacker-signed, on the same struct as `body`,
/// travelling the exact same path to the ledger) or the host-resolved
/// stream excerpt from [`resolve_stream_excerpt`] (the watcher picks the
/// index; the host supplies the referenced conversation event's own text,
/// which can itself be arbitrarily large — a remote-chosen index reaching
/// into host-side data is not "just metadata"). 2 KiB is generous for
/// quoted supporting context — an excerpt is not the message itself —
/// while keeping either source from reaching the ledger unbounded.
const MAX_ANNOTATION_EXCERPT_BYTES: usize = 2048;

/// Hard cap on the anchor-location bytes [`format_steer`] renders into a
/// block's header line: [`junto_kernel::CodeAnchor::path`] and
/// [`junto_kernel::StreamAnchor::op_id`]. Both are attacker-signed
/// `String`s the kernel does not length-bound — `op_id` is documented on
/// the type itself as "opaque… its shape is the live-document layer's
/// concern," and `path` carries no length check in
/// `validate_annotation_update` either. Without this cap a single
/// annotation with an oversized `path` or `op_id`, an empty excerpt, and a
/// minimal `body` would still let ONE comment fill the frame and become a
/// multi-megabyte immutable entry — reachable with one annotation instead
/// of the many [`MAX_STEER_MESSAGE_BYTES`] guards against, because
/// [`assemble_batch`] always renders the first block regardless of size.
/// 256 bytes is generous for any real repo-relative path or CRDT op id.
const MAX_ANCHOR_LOCATION_BYTES: usize = 256;

/// Hard cap on the TOTAL bytes of one rendered steer message, including
/// [`assemble_batch`]'s own withheld-count marker when one is appended —
/// see [`MARKER_RESERVE_BYTES`] for how the marker is kept inside this
/// budget rather than added on top of it. [`MAX_ANNOTATION_BODY_BYTES`],
/// [`MAX_ANNOTATION_EXCERPT_BYTES`], and [`MAX_ANCHOR_LOCATION_BYTES`]
/// bound a single annotation's contribution (together, well under this
/// budget — see [`assemble_batch`]'s doc comment for the measured worst
/// case), but [`deliver_batch`] renders an entire batch into ONE ledger
/// note, and an accepted (already individually-legal) 8 MiB websocket
/// frame can still carry an unbounded NUMBER of annotations — roughly
/// 2000 individually-legal 4 KiB bodies in one frame would otherwise
/// still produce one multi-megabyte immutable ledger entry, because
/// nothing bounded the *batch*, only each annotation inside it. 16 KiB
/// keeps a genuinely busy review's worth of comments in one steer message
/// while keeping the ledger entry itself finite regardless of batch size.
const MAX_STEER_MESSAGE_BYTES: usize = 16384;

/// Bytes of [`MAX_STEER_MESSAGE_BYTES`] reserved for [`assemble_batch`]'s
/// own withheld-count marker, so the FINISHED message (rendered blocks
/// *plus* the marker, when one is appended) never exceeds the budget —
/// only the blocks portion would if the marker were appended on top of a
/// full budget instead of carved out of it. This guarantee holds as long
/// as every individual block stays under the reduced (budget minus this
/// reserve) block budget, which is exactly what the FIRST-block clause in
/// [`assemble_batch`]'s doc comment exists to survive losing — today the
/// field caps ensure it with the measured ~5.4 KB margin documented
/// there, but that margin, not this reservation alone, is what makes the
/// guarantee hold. The marker's own variable content is just the withheld
/// count and the (fixed, 5-digit) budget number; even an absurd withheld
/// count of one billion renders under 200 bytes, so 300 leaves
/// comfortable headroom without meaningfully shrinking the room left for
/// actual comment content.
const MARKER_RESERVE_BYTES: usize = 300;

/// Shared truncation: cut `value` to at most `max` bytes on a UTF-8 char
/// boundary and append an explicit marker naming what was cut. A silently
/// shortened comment is worse than a visibly truncated one — the agent
/// must always be able to tell.
fn truncate_with_marker(value: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if value.len() <= max {
        return std::borrow::Cow::Borrowed(value);
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!(
        "{}\n…[truncated: {end} of {} bytes shown]",
        &value[..end],
        value.len()
    ))
}

/// `body`, truncated to at most [`MAX_ANNOTATION_BODY_BYTES`] — see
/// [`truncate_with_marker`].
fn capped_body(body: &str) -> std::borrow::Cow<'_, str> {
    truncate_with_marker(body, MAX_ANNOTATION_BODY_BYTES)
}

/// An excerpt (`annotation.excerpt` or a resolved stream excerpt),
/// truncated to at most [`MAX_ANNOTATION_EXCERPT_BYTES`] — see
/// [`truncate_with_marker`]. Applied by [`format_steer`] to whichever
/// source it ends up quoting, so neither can smuggle an unbounded quote
/// block into the ledger note under cover of "just the excerpt".
fn capped_excerpt(excerpt: &str) -> std::borrow::Cow<'_, str> {
    truncate_with_marker(excerpt, MAX_ANNOTATION_EXCERPT_BYTES)
}

/// An anchor-location field (`CodeAnchor.path` or `StreamAnchor.op_id`),
/// truncated to at most [`MAX_ANCHOR_LOCATION_BYTES`] — see
/// [`truncate_with_marker`]. Applied by [`format_steer`] before either
/// field is formatted into the block header, so an oversized path or op
/// id can't reach the ledger through the one field this branch's caps
/// used to leave open.
fn capped_location(location: &str) -> std::borrow::Cow<'_, str> {
    truncate_with_marker(location, MAX_ANCHOR_LOCATION_BYTES)
}

/// Assemble already-rendered, in-order per-annotation `blocks` (each one
/// already bounded by [`MAX_ANNOTATION_BODY_BYTES`],
/// [`MAX_ANNOTATION_EXCERPT_BYTES`], and [`MAX_ANCHOR_LOCATION_BYTES`]
/// before any of them ever reaches this function — measured worst case
/// for one block with every capped field maxed out and an `Orphaned`
/// re-anchor: ~10.7 KB, well under the budget below) into one steer
/// message capped at [`MAX_STEER_MESSAGE_BYTES`] total, marker included.
///
/// Renders whole blocks, in order, against a budget already reduced by
/// [`MARKER_RESERVE_BYTES`] (so the withheld-count marker, if one ends up
/// appended, still fits inside [`MAX_STEER_MESSAGE_BYTES`] rather than
/// pushing the total over it), until adding the next one would exceed
/// that reduced budget, then stops — a block is never partially rendered
/// here. The FIRST block always renders in full regardless of its own
/// size: a batch must never render as nothing but a withheld-count. Today
/// that clause is a defensive fallback, not a reachable path — every
/// block's own field caps bound it to well under the (reduced) budget, as
/// measured above — but if a future cap change ever let a single block
/// exceed the budget, this function still renders it whole rather than
/// mangling it further.
///
/// Any blocks left over are never silently lost: they are already durable
/// in the session's `LiveDoc` and, at turn end, in that turn's archived
/// `turn-<n>-live.loro` snapshot artifact (see
/// `crate::launch::archive_live_snapshot`) — only THIS steer message is
/// bounded, not the record of the annotations themselves. The withheld
/// count and that fact are both stated in an explicit trailing line.
fn assemble_batch(blocks: Vec<String>) -> String {
    let block_budget = MAX_STEER_MESSAGE_BYTES.saturating_sub(MARKER_RESERVE_BYTES);
    let mut out = String::new();
    let mut included = 0usize;
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            let projected = out.len() + 1 + block.len();
            if projected > block_budget {
                break;
            }
            out.push('\n');
        }
        out.push_str(block);
        included += 1;
    }
    let withheld = blocks.len() - included;
    if withheld > 0 {
        out.push_str(&format!(
            "\n[{withheld} further comment(s) withheld from this steer message to stay \
             within the {MAX_STEER_MESSAGE_BYTES}-byte budget — not lost: they remain in \
             the live document and its archived turn-<n>-live.loro artifact.]\n"
        ));
    }
    out
}

/// Resolve a [`junto_kernel::StreamAnchor`]'s `op_id` back to the
/// conversation event it names, for [`format_steer`] to quote as the
/// excerpt when the annotation didn't pin one itself (IMPORTANT 5 of the
/// final branch review). The shipped composer (`junto-iced`) sets `op_id`
/// to the `conversation` container's own index and never sets `excerpt`,
/// so without this every stream-anchored comment reached the agent as a
/// bare, unresolvable event number with no quoted content at all. `None`
/// when `op_id` doesn't parse as an index, `live_doc` is unavailable, or
/// the index doesn't resolve. Prefers the event's raw `markdown` source
/// over its (possibly sanitized HTML) `text` field — see
/// `crate::launch::LiveEvent`.
fn resolve_stream_excerpt(live_doc: Option<&LiveDoc>, op_id: &str) -> Option<String> {
    let index: usize = op_id.parse().ok()?;
    let event = live_doc?.conversation_event(index)?;
    event
        .get("markdown")
        .and_then(|v| v.as_str())
        .or_else(|| event.get("text").and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Render a batch of annotations as one steer message for the driving
/// agent's context.
///
/// `reanchored[i]` is the current position of `annotations[i]`'s anchor
/// (`None` when re-anchoring wasn't attempted or failed — see
/// [`reanchor_batch`]); the two slices are parallel by index, matching how
/// [`deliver_batch`] builds them together. `live_doc` is the session's
/// live document, when one is registered — used only to resolve a
/// `Anchor::Stream` annotation's referenced event (see
/// [`resolve_stream_excerpt`]); `None` degrades every stream anchor to the
/// "could not resolve" wording rather than panicking or fabricating
/// content.
///
/// The agent has no other view of the watcher's screen, so each block names
/// **who** commented, **where** (the pinned `path:span`, plus where that
/// span sits now if it moved), quotes an **excerpt** — the annotation's own
/// pinned one if it carried one, otherwise (for a stream anchor) the
/// referenced conversation event's own text — every line of it prefixed so
/// a multi-line span (the normal case: a `CodeAnchor` pins a line *range*)
/// reads as one quoted block rather than bleeding into the comment that
/// follows — because the current file content may no longer match what the
/// watcher was actually looking at. An `Orphaned` re-anchor says so plainly
/// rather than pointing at a line range that no longer corresponds to
/// anything the watcher commented on; an unresolvable stream anchor says so
/// plainly too, rather than emitting a bare, meaningless event number.
///
/// Whichever excerpt wins is capped by [`capped_excerpt`], `body` by
/// [`capped_body`], and the anchor's own `path`/`op_id` by
/// [`capped_location`] before any of them reaches the block; the finished
/// blocks are then handed to [`assemble_batch`], which bounds the WHOLE
/// rendered batch at [`MAX_STEER_MESSAGE_BYTES`] — all four caps together
/// are what keep this function's output finite, not any one of them alone.
pub(crate) fn format_steer(
    annotations: &[Annotation],
    reanchored: &[Option<Reanchor>],
    live_doc: Option<&LiveDoc>,
) -> String {
    let blocks: Vec<String> = annotations
        .iter()
        .enumerate()
        .map(|(i, annotation)| {
            let mut block = String::new();
            let current = reanchored.get(i).copied().flatten();
            // Resolved once per annotation: it doubles as the location note
            // (whether the referenced event could be found at all) and, absent
            // a pinned `excerpt`, as the excerpt itself.
            let stream_excerpt = match &annotation.anchor {
                Anchor::Stream(stream) => resolve_stream_excerpt(live_doc, &stream.op_id),
                Anchor::Code(_) | Anchor::Record(_) => None,
            };
            let location = match &annotation.anchor {
                Anchor::Code(code) => {
                    let path = capped_location(&code.path);
                    let pinned = format!("{path}:{}-{}", code.span.start, code.span.end);
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
                // no re-anchored *position* to report — but the referenced
                // event itself can still fail to resolve (unparseable `op_id`,
                // no `LiveDoc` in hand, or an out-of-range index), and that
                // must be said plainly rather than left implicit in a bare
                // number the agent has no way to act on. Capped here too: an
                // `op_id` that fails to parse as an index (so `stream_excerpt`
                // is `None`) still reaches this "could not resolve" branch
                // and gets displayed raw otherwise.
                Anchor::Stream(stream) if stream_excerpt.is_some() => {
                    format!("on conversation event {}", capped_location(&stream.op_id))
                }
                Anchor::Stream(stream) => format!(
                    "on conversation event {} (could not resolve — the event is unavailable)",
                    capped_location(&stream.op_id)
                ),
                // Record content is immutable and digest-pinned, so there is
                // nothing to re-anchor and no drift note to make — the span
                // means now exactly what it meant when it was written. The
                // entry id is what the agent needs to find the content again.
                Anchor::Record(record) => format!(
                    "on lines {}-{} of {}",
                    record.span.start,
                    record.span.end,
                    capped_location(&record.entry.to_string())
                ),
            };
            let on = if matches!(annotation.anchor, Anchor::Stream(_)) {
                location
            } else {
                format!("on {location}")
            };
            block.push_str(&format!(
                "[watcher comment — {} {on}]\n",
                annotation.author.email
            ));
            // The annotation's own pinned excerpt wins when present — it
            // travels with the comment because the current file content may no
            // longer match what the watcher was actually looking at. A
            // resolved stream event is the fallback, not a replacement. Either
            // way it is capped by `capped_excerpt` — a remote peer's pinned
            // excerpt and a host-resolved event's text are equally capable of
            // being unbounded.
            let excerpt = annotation.excerpt.as_deref().or(stream_excerpt.as_deref());
            if let Some(excerpt) = excerpt {
                let excerpt = capped_excerpt(excerpt);
                // Every line quoted, not just the first: a `CodeAnchor`'s span
                // is a line *range* (the excerpt is normally multi-line), and
                // an unmarked line reads to the model as the start of the
                // comment body rather than still-quoted code. This also quotes
                // a truncation marker's own line when one was appended, which
                // is correct: it is part of the excerpt block being shown.
                for line in excerpt.lines() {
                    block.push_str("> ");
                    block.push_str(line);
                    block.push('\n');
                }
            }
            block.push_str(&capped_body(&annotation.body));
            block.push('\n');
            block
        })
        .collect();
    assemble_batch(blocks)
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
    // never a reason to withhold the comment itself. Keep the `Arc` alive
    // (not just its workspace clone) for the whole function: `format_steer`
    // below also borrows `live.doc` to resolve stream-anchor excerpts.
    let live = host.live_plane().get(session);
    let workspace = live.as_ref().and_then(|live| {
        live.workspace
            .lock()
            .expect("live plane workspace lock")
            .clone()
    });
    let reanchored = reanchor_batch(workspace.as_deref(), &annotations).await;
    let message = format_steer(&annotations, &reanchored, live.as_deref().map(|s| &s.doc));
    // The batch becomes one steer message, so the ledger's steer note
    // records a single author: the first annotation's, in batch order —
    // subject to IMPORTANT 4 of the final branch review, below.
    let sender = first.author.clone();
    // `steer_live` -> `record_steer_note` -> `append` -> `Host::sign_entry`
    // resolves (and, on a miss, MINTS AND PERSISTS) a signing key for
    // whatever email is passed as `author`. `sender` is a REMOTE watcher's
    // `Member`; using it unconditionally would silently write a new
    // private key for someone else's identity into THIS machine's
    // `keys.toml` the moment the driving host isn't the one that granted
    // that watcher's membership — and the entry would still land in
    // `ChannelView::unverified` (ADR 0033) anyway, since a freshly-minted
    // key here is never in the channel's keyring. Author as `sender` only
    // when this host already holds a key for that identity (the correct,
    // common case); otherwise author as the session's own driving agent
    // and name the watcher in the note text instead, so nothing is lost.
    // `host.member_home()` (not the global `crate::host::junto_home()`)
    // is deliberate: it is the exact path `sign_entry` itself signs
    // against, override included — checking a different home would check
    // the wrong store.
    let member_home = host.member_home().ok();
    let sender_has_local_key = member_home
        .as_deref()
        .and_then(|home| crate::keys::has_signing_key(home, &sender.email).ok())
        .unwrap_or(false);
    let (steered_by, message) = if sender_has_local_key {
        (sender, message)
    } else {
        // The driving agent config store (`agents.toml`,
        // `harness_sessions.json`) is the machine-wide junto home, not
        // `member_home` — the same distinction `crate::launch`'s own
        // callers (`steer`, `record_outcome`) already draw. `resume_agent`
        // fails only if that home can't even be read, in which case
        // `sign_entry`'s own key lookup below would fail identically, so
        // this fallback introduces no new failure mode.
        let driving_agent = crate::host::junto_home()
            .ok()
            .and_then(|home| crate::launch::resume_agent(&home, &session).ok())
            .map(|agent| agent.member())
            .unwrap_or_else(|| Member::agent("junto live bridge", "live-bridge@junto.local"));
        (
            driving_agent,
            format!(
                "(relayed on behalf of watcher {}, who has no local signing key on this host) {message}",
                sender.email
            ),
        )
    };
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
            None,
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
        let out = format_steer(&[ann], &[Some(Reanchor::Orphaned)], None);
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
            None,
        );
        assert!(out.contains("src/foo.rs:5-5"));
        assert!(!out.contains("moved to"));
        assert!(!out.contains("changed since"));
    }

    #[test]
    fn batch_concatenates_in_order() {
        let a = code_annotation("a@x.com", "a.rs", 1, 1, None, "first");
        let b = code_annotation("b@x.com", "b.rs", 2, 2, None, "second");
        let out = format_steer(&[a, b], &[None, None], None);
        assert!(out.find("first").unwrap() < out.find("second").unwrap());
    }

    #[test]
    fn missing_excerpt_omits_the_quote_block() {
        let ann = code_annotation("a@x.com", "a.rs", 1, 1, None, "no excerpt here");
        let out = format_steer(&[ann], &[None], None);
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
        let out = format_steer(&[ann], &[None], None);
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
        let out = format_steer(&[ann], &[None], None);
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

    #[test]
    fn oversized_body_is_truncated_with_a_marker() {
        // IMPORTANT 3 of the final branch review: `annotation.body` is the
        // one path by which a remote peer's own bytes reach the durable
        // ledger via `steer_live`'s `note: format!("steer: {message}")`.
        let huge = "x".repeat(MAX_ANNOTATION_BODY_BYTES + 500);
        let ann = code_annotation("a@x.com", "a.rs", 1, 1, None, &huge);
        let out = format_steer(&[ann], &[None], None);
        assert!(
            out.len() < huge.len(),
            "the rendered output must be shorter than the oversized body"
        );
        assert!(
            out.contains("truncated"),
            "a truncated body must carry an explicit marker: {out}"
        );
        assert!(
            out.contains(&"x".repeat(MAX_ANNOTATION_BODY_BYTES)),
            "exactly the cap's worth of the original bytes must survive"
        );
    }

    #[test]
    fn oversized_excerpt_is_truncated_with_a_marker() {
        // Residual from the final branch review: `annotation.excerpt` is a
        // remote-supplied, attacker-signed field on the same struct as
        // `body`, travelling the exact same path to the ledger — the body
        // cap alone left this one unbounded.
        let huge_excerpt = "e".repeat(MAX_ANNOTATION_EXCERPT_BYTES + 500);
        let ann = code_annotation("a@x.com", "a.rs", 1, 1, Some(&huge_excerpt), "short body");
        let out = format_steer(&[ann], &[None], None);
        assert!(
            out.len() < huge_excerpt.len(),
            "the rendered output must be shorter than the oversized excerpt alone"
        );
        assert!(
            out.contains("truncated"),
            "a truncated excerpt must carry an explicit marker: {out}"
        );
        assert!(
            out.contains(&format!("> {}", "e".repeat(MAX_ANNOTATION_EXCERPT_BYTES))),
            "exactly the cap's worth of the original excerpt bytes must survive, quoted: {out}"
        );
        assert!(
            out.contains("short body"),
            "the body must still render: {out}"
        );
    }

    #[test]
    fn body_within_the_cap_is_rendered_verbatim_and_unmarked() {
        let body = "well within the cap";
        let ann = code_annotation("a@x.com", "a.rs", 1, 1, None, body);
        let out = format_steer(&[ann], &[None], None);
        assert!(out.contains(body));
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn resolvable_stream_anchor_quotes_the_referenced_event() {
        // IMPORTANT 5 of the final branch review.
        let doc = LiveDoc::new();
        doc.push_conversation(&serde_json::json!({
            "kind": "assistant",
            "text": "<p>hi there</p>",
            "markdown": "hi there",
        }));
        let ann = Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", "dan@x.com"),
            anchor: Anchor::Stream(junto_kernel::StreamAnchor {
                session: EntryId::new(),
                op_id: "0".into(),
            }),
            body: "what's this doing?".into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        };
        let out = format_steer(&[ann], &[None], Some(&doc));
        assert!(out.contains("on conversation event 0"));
        assert!(!out.contains("could not resolve"), "{out}");
        assert!(
            out.contains("> hi there"),
            "the resolved event's own text must be quoted as the excerpt: {out}"
        );
    }

    #[test]
    fn oversized_resolved_stream_excerpt_is_truncated_with_a_marker() {
        // The annotation carries no `excerpt` of its own; the referenced
        // conversation event's own text is huge. This excerpt is
        // host-resolved, not remote-supplied directly, but the index
        // choosing which event to resolve IS remote-chosen, and the
        // resulting text still reaches the ledger the same way a pinned
        // excerpt would — it must be capped identically.
        let huge_text = "s".repeat(MAX_ANNOTATION_EXCERPT_BYTES + 500);
        let doc = LiveDoc::new();
        doc.push_conversation(&serde_json::json!({
            "kind": "assistant",
            "text": huge_text.clone(),
            "markdown": huge_text.clone(),
        }));
        let ann = Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", "dan@x.com"),
            anchor: Anchor::Stream(junto_kernel::StreamAnchor {
                session: EntryId::new(),
                op_id: "0".into(),
            }),
            body: "what's this doing?".into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        };
        let out = format_steer(&[ann], &[None], Some(&doc));
        assert!(
            out.len() < huge_text.len(),
            "the rendered output must be shorter than the oversized resolved excerpt alone"
        );
        assert!(
            out.contains("truncated"),
            "a truncated resolved stream excerpt must carry an explicit marker: {out}"
        );
        assert!(out.contains("on conversation event 0"));
    }

    #[test]
    fn unresolvable_stream_anchor_says_so_plainly() {
        // Empty doc: index 0 doesn't exist. Must not silently emit a bare,
        // meaningless event number as if it were resolved.
        let doc = LiveDoc::new();
        let ann = Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", "dan@x.com"),
            anchor: Anchor::Stream(junto_kernel::StreamAnchor {
                session: EntryId::new(),
                op_id: "0".into(),
            }),
            body: "what's this doing?".into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        };
        let out = format_steer(&[ann], &[None], Some(&doc));
        assert!(
            out.contains("could not resolve"),
            "an unresolvable stream anchor must say so plainly: {out}"
        );
        assert!(
            !out.contains('>'),
            "no excerpt block when nothing resolved: {out}"
        );
    }

    #[tokio::test]
    async fn unknown_remote_author_never_gets_a_minted_key() {
        // IMPORTANT 4 of the final branch review: `steered_by` used to be
        // `first.author` unconditionally, handed straight to `steer_live`
        // -> `record_steer_note` -> `append` -> `Host::sign_entry`, which
        // mints and persists a signing key on a miss. A remote watcher
        // this host never granted membership to must never get a key
        // minted for their email on this machine.
        let (host, channel_ref, _dir, member_home) = host_with_channel().await;
        let session = EntryId::new();
        // Steerable and registered, so `steer_live` actually reaches
        // `record_steer_note`/`append`/`sign_entry` instead of stopping at
        // `NotLive` — this test must prove the mint never happens even
        // when the note genuinely gets recorded.
        let _control_rx = host
            .live()
            .begin(Arc::clone(&host), channel_ref.clone(), session, true);

        let mut annotation = code_annotation(
            "unknown@remote.example.com",
            "src/foo.rs",
            1,
            1,
            None,
            "please look",
        );
        annotation.urgent = true;

        deliver(Arc::clone(&host), channel_ref, session, vec![annotation]).await;

        assert!(
            !crate::keys::has_signing_key(member_home.path(), "unknown@remote.example.com")
                .unwrap(),
            "an unidentified remote watcher must never get a locally-minted signing key"
        );
    }

    #[test]
    fn batch_of_many_legal_annotations_stays_within_the_message_budget() {
        // Per-annotation caps are per-annotation; the ledger note is
        // per-batch. A frame can carry an unbounded NUMBER of
        // individually-legal annotations, and `deliver_batch` renders the
        // whole vector into ONE message — this must stay bounded, and
        // nothing withheld may vanish silently.
        let n = 40;
        let annotations: Vec<Annotation> = (0..n)
            .map(|i| {
                code_annotation(
                    "a@x.com",
                    "a.rs",
                    1,
                    1,
                    None,
                    &format!("legal body #{i}: {}", "y".repeat(1000)),
                )
            })
            .collect();
        let reanchored = vec![None; n];
        let out = format_steer(&annotations, &reanchored, None);

        let included = out.matches("[watcher comment").count();
        assert!(
            included < n,
            "a large-enough legal batch must not all fit within the message budget: {included} of {n} rendered"
        );
        assert!(included >= 1, "at least one comment must always render");

        let withheld = n - included;
        assert!(
            out.contains(&format!("{withheld} further comment")),
            "the exact withheld count must be stated explicitly: {out}"
        );
        assert!(
            out.contains("live document") && out.contains("archived"),
            "withheld comments must be stated as retained, not lost: {out}"
        );
        assert!(
            out.len() <= MAX_STEER_MESSAGE_BYTES,
            "the assembled message (blocks plus the withheld-count marker) must never \
             exceed the byte budget itself — MARKER_RESERVE_BYTES exists precisely so \
             the marker never pushes it over: {} bytes",
            out.len()
        );
    }

    #[test]
    fn oversized_code_path_is_truncated_and_the_message_stays_bounded() {
        // A single annotation, reachable as `assemble_batch`'s unconditional
        // first block, with an oversized `CodeAnchor.path`, no excerpt, and
        // a minimal body: before the anchor-location cap, this path alone
        // could fill the frame and reach the ledger as one multi-megabyte
        // entry — no second annotation required.
        let huge_path = "p".repeat(MAX_ANCHOR_LOCATION_BYTES + 500);
        let ann = code_annotation("a@x.com", &huge_path, 1, 1, None, "short body");
        let out = format_steer(&[ann], &[None], None);
        assert!(
            out.len() < huge_path.len(),
            "the rendered output must be shorter than the oversized path alone"
        );
        assert!(
            out.contains("truncated"),
            "a truncated path must carry an explicit marker: {out}"
        );
        assert!(
            out.contains("short body"),
            "the body must still render: {out}"
        );
    }

    #[test]
    fn oversized_stream_op_id_is_truncated_and_the_message_stays_bounded() {
        // Same vector as the path test, on the stream-anchor side: `op_id`
        // is documented as opaque and kernel-unvalidated. A non-numeric
        // huge `op_id` fails `resolve_stream_excerpt`'s `parse::<usize>()`
        // and falls into the "could not resolve" branch, which — before
        // this cap — displayed the raw, unbounded `op_id` verbatim.
        let huge_op_id = "o".repeat(MAX_ANCHOR_LOCATION_BYTES + 500);
        let ann = Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", "dan@x.com"),
            anchor: Anchor::Stream(junto_kernel::StreamAnchor {
                session: EntryId::new(),
                op_id: huge_op_id.clone(),
            }),
            body: "short body".into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        };
        let out = format_steer(&[ann], &[None], None);
        assert!(
            out.len() < huge_op_id.len(),
            "the rendered output must be shorter than the oversized op_id alone"
        );
        assert!(
            out.contains("truncated"),
            "a truncated op_id must carry an explicit marker: {out}"
        );
        assert!(
            out.contains("could not resolve"),
            "a non-numeric op_id must still say it could not resolve: {out}"
        );
        assert!(
            out.contains("short body"),
            "the body must still render: {out}"
        );
    }

    #[test]
    fn single_block_over_budget_still_renders_whole() {
        // Decision 4: a batch must never render as nothing but a
        // withheld-count. With all four field caps in place (body,
        // excerpt, and now anchor location), a lone block's measured worst
        // case is ~10.7 KB — well under the ~16.1 KB reduced block budget
        // (MAX_STEER_MESSAGE_BYTES minus MARKER_RESERVE_BYTES) — so this
        // branch is a defensive fallback for a future cap change, not a
        // reachable path today. Exercised directly against `assemble_batch`
        // (not `format_steer`) because no combination of legal, capped
        // field values can actually produce a block this large.
        let huge_block = format!(
            "[watcher comment — a@x.com on a.rs:1-1]\n{}\n",
            "z".repeat(MAX_STEER_MESSAGE_BYTES + 1000)
        );
        let out = assemble_batch(vec![huge_block.clone()]);
        assert_eq!(
            out, huge_block,
            "the sole block must render in full, unmodified by the batch assembler"
        );
        assert!(
            !out.contains("withheld"),
            "nothing was withheld when it is the only block: {out}"
        );
    }
}

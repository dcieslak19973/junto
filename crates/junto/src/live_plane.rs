//! The host's per-session live plane registry (`docs/superpowers/specs/2026-08-20-live-session-plane-design.md`).
//!
//! `junto-live` gives us one CRDT document per running Agent Session; this
//! module is where the host actually keeps those documents while a session
//! runs, and tears them down when it ends. [`LivePlane`] sits beside
//! [`crate::launch::LiveSessions`] (the existing SSE progress feed) as a
//! second, richer registry — [`crate::launch::LiveSessions`] taps it
//! directly from `begin`/`publish`/`finish` (see that type's module docs)
//! rather than replacing anything already there.
//!
//! **The invariant this whole module exists to uphold: the live plane may
//! never block, slow, or corrupt the session loop.** Every tap into a
//! [`SessionLive`] is fire-and-forget — a serialization failure or a full
//! broadcast channel is logged and dropped, never propagated into the
//! turn-driving `async fn`s in `crate::launch`. A panic or a stalled await in
//! a tap would take down a running agent turn; nothing here may ever do
//! that.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use junto_kernel::EntryId;
use junto_live::{Frame, LiveDoc, Presence};
use tokio::sync::broadcast;

/// One live agent session's CRDT state, ephemeral watcher presence, and the
/// outbound [`Frame`] broadcast every connected watcher subscribes to.
/// Constructed once per session by [`LivePlane::begin`] and archived by
/// [`LivePlane::finish`] — see the module docs for the taps that drive it.
///
/// Loro handles (`LiveDoc`, `Presence`) are internally synchronized and
/// `Send + Sync`, so this type is shared as `Arc<SessionLive>` between the
/// session loop and (later) watcher connections; it is never wrapped in its
/// own `Mutex`.
pub(crate) struct SessionLive {
    /// The session's live CRDT document: `conversation`, `worktree`, and
    /// `annotations` containers (`junto_live::doc`).
    pub doc: LiveDoc,
    /// Who is currently watching this session (`junto_live::presence`).
    /// Unread by this task's taps — the watcher-presence heartbeat lands in
    /// a later task.
    #[allow(dead_code)]
    pub presence: Presence,
    /// Every [`Frame`] this session produces, for connected watchers to
    /// subscribe to (the websocket handler, a later task). Capacity 256: a
    /// slow watcher drops frames rather than backing up the sender — joining
    /// from a fresh snapshot is the recovery path, not replay.
    pub outbound: broadcast::Sender<Frame>,
    /// Watcher annotations validated but not yet delivered to the driving
    /// agent as steering context (the annotation→steer bridge, a later
    /// task). Unread by this task's taps.
    #[allow(dead_code)]
    pub pending: Mutex<Vec<junto_kernel::Annotation>>,
    /// The session's workspace path, for re-anchoring annotations before
    /// they're delivered as steering context (the annotation→steer bridge,
    /// a later task). Left `None` by every tap in this task — declared here
    /// once so that later task doesn't need a second edit to this struct.
    #[allow(dead_code)]
    pub workspace: Mutex<Option<PathBuf>>,
    /// Keeps `doc`'s local-update subscription alive for as long as this
    /// session lives; the subscription itself forwards every local commit
    /// into `outbound` as a `Frame::Update` (wired in [`LivePlane::begin`]).
    /// Never read after construction — its only job is to not be dropped.
    /// Type-erased so this module never needs to name `loro`'s
    /// `Subscription` type directly (`junto` does not depend on `loro`;
    /// only `junto-live` does).
    _subscription: Box<dyn std::any::Any + Send + Sync>,
}

impl SessionLive {
    /// Tap one conversation event from the existing SSE live feed
    /// (`crate::launch::LiveEvent`) into this session's CRDT document.
    /// Returns the event's serialized JSON value on success, so a caller
    /// that also needs to push the same event into `worktree` (a tool
    /// event whose label indicates a file edit or write — see
    /// `crate::launch::LiveSessions::publish`) can pass that same value by
    /// reference to `doc.push_worktree` instead of serializing `event` a
    /// second time — `LiveDoc::push_conversation`/`push_worktree` both take
    /// `&serde_json::Value`, so no clone is needed for either push.
    ///
    /// Fire-and-forget: a serialization failure is logged and dropped, never
    /// propagated — see the module docs on the never-block invariant. In
    /// practice `LiveEvent` always serializes (plain strings, a `u64`, a
    /// `bool`); this only guards against that ceasing to be true.
    pub(crate) fn publish_conversation(
        &self,
        event: &crate::launch::LiveEvent,
    ) -> Option<serde_json::Value> {
        match serde_json::to_value(event) {
            Ok(value) => {
                self.doc.push_conversation(&value);
                Some(value)
            }
            Err(err) => {
                tracing::warn!("live plane: failed to serialize conversation event: {err:#}");
                None
            }
        }
    }
}

/// The host's per-session registry of live plane state — one [`SessionLive`]
/// per running Agent Session. Tapped from [`crate::launch::LiveSessions`]
/// (`begin`/`publish`/`finish`) alongside the existing SSE progress feed;
/// see that type's module docs for how the two registries relate.
///
/// Ephemeral, like `LiveSessions`: nothing here is part of the durable
/// record. [`LivePlane::finish`] hands back the session's final CRDT
/// snapshot bytes for the caller to archive as an Artifact — this module
/// never touches the ledger itself.
#[derive(Default)]
pub(crate) struct LivePlane {
    sessions: Mutex<HashMap<EntryId, Arc<SessionLive>>>,
}

impl LivePlane {
    /// Start a fresh [`SessionLive`] for `session`, wiring its document's
    /// local-update subscription to forward every commit into `outbound` as
    /// a `Frame::Update`.
    ///
    /// Replacing any stale entry (a turn's task ended without calling
    /// [`LivePlane::finish`] — e.g. it panicked, or the process restarted
    /// mid-turn — and the session was then re-launched) broadcasts
    /// `Frame::End` on the *removed* entry's own `outbound` first, exactly
    /// as `finish` would have: a watcher still holding that old
    /// `broadcast::Receiver` must learn its stream is over rather than
    /// silently hang on a sender nothing will ever send through again.
    pub(crate) fn begin(&self, session: EntryId) -> Arc<SessionLive> {
        let doc = LiveDoc::new();
        let (outbound, _rx) = broadcast::channel(256);
        let forward = outbound.clone();
        let subscription = doc.subscribe_local_update(move |bytes| {
            // No watcher connected right now is not an error — fire and
            // forget, same as every other tap in this module.
            let _ = forward.send(Frame::update(bytes));
            true
        });
        let live = Arc::new(SessionLive {
            doc,
            presence: Presence::new(),
            outbound,
            pending: Mutex::new(Vec::new()),
            workspace: Mutex::new(None),
            _subscription: Box::new(subscription),
        });
        let stale = self
            .sessions
            .lock()
            .expect("live plane registry lock")
            .insert(session, Arc::clone(&live));
        if let Some(stale) = stale {
            let _ = stale.outbound.send(Frame::End);
        }
        live
    }

    /// The live state for a running session, or `None` if it isn't live
    /// (finished already, or never began).
    #[must_use]
    pub(crate) fn get(&self, session: EntryId) -> Option<Arc<SessionLive>> {
        self.sessions
            .lock()
            .expect("live plane registry lock")
            .get(&session)
            .cloned()
    }

    /// End a session: remove it from the registry, broadcast `Frame::End` to
    /// any connected watchers, and return its final CRDT snapshot bytes for
    /// the caller to archive — `None` if the session wasn't live.
    pub(crate) fn finish(&self, session: EntryId) -> Option<Vec<u8>> {
        let live = self
            .sessions
            .lock()
            .expect("live plane registry lock")
            .remove(&session)?;
        // Fire-and-forget: no watcher connected is not an error.
        let _ = live.outbound.send(Frame::End);
        Some(live.doc.export_snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bare `LiveEvent` for the test below — `crate::launch::LiveEvent`
    /// is the type `SessionLive::publish_conversation` taps, but its
    /// constructors are `pub(crate)`, not exported for external test crates.
    fn test_live_event(kind: &str, text: &str) -> crate::launch::LiveEvent {
        crate::launch::LiveEvent::new(kind, text)
    }

    #[tokio::test]
    async fn plane_lifecycle_publishes_and_archives() {
        let plane = LivePlane::default();
        let session = junto_kernel::EntryId::new();
        let live = plane.begin(session);
        let mut rx = live.outbound.subscribe();
        live.publish_conversation(&test_live_event("assistant", "hello"));
        let frame = rx.recv().await.unwrap();
        assert!(matches!(frame, junto_live::Frame::Update { .. }));
        assert_eq!(live.doc.conversation_len(), 1);
        let snapshot = plane.finish(session).expect("snapshot");
        let replay = junto_live::LiveDoc::new();
        replay.import_update(&snapshot).unwrap();
        assert_eq!(replay.conversation_len(), 1);
        assert!(
            replay.conversation_matches(&live.doc),
            "the archived snapshot must replay the exact event, not just its count"
        );
        assert!(plane.get(session).is_none());
    }
}

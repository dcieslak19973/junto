//! The annotation → steer bridge: hands validated watcher annotations to the
//! driving agent as steering context.
//!
//! **This module is a stub.** [`deliver`]'s signature is fixed by
//! `docs/superpowers/specs/2026-08-20-live-session-plane-design.md` Task 7 so
//! [`crate::live_ws`] has a stable call site; the body — re-anchoring an
//! annotation's [`junto_kernel::Anchor`] against the session's current
//! worktree state, splitting urgent from non-urgent, and actually steering a
//! running turn — is Task 8's job. Until then this only appends to
//! [`crate::live_plane::SessionLive::pending`], exactly the queue Task 8's
//! steering pass will drain.
use std::sync::Arc;

use junto_kernel::{Annotation, EntryId};

use crate::host::Host;

/// Hand newly validated watcher `annotations` for `session` (in `channel`) to
/// the steering bridge.
///
/// Fire-and-forget, like every other live-plane tap (`crate::live_plane`
/// module docs: the never-block invariant) — a session that ended between
/// validation and this call is not an error, just nothing left to append to.
pub(crate) async fn deliver(
    host: Arc<Host>,
    channel: String,
    session: EntryId,
    annotations: Vec<Annotation>,
) {
    let _ = channel;
    if let Some(live) = host.live_plane().get(session) {
        live.pending
            .lock()
            .expect("live plane pending-annotations lock")
            .extend(annotations);
    }
}

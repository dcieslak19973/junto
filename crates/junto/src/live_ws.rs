//! The authenticated live websocket endpoint (`docs/superpowers/specs/
//! 2026-08-20-live-session-plane-design.md` Task 7): remote watchers connect
//! at `/channels/{channel}/sessions/{session}/live`, prove their identity
//! against the channel's keyring with an ed25519 challenge-response, receive
//! the session's live document as a snapshot, and may submit annotations —
//! validated by [`junto_live::validate_annotation_update`] before anything
//! they send ever touches the real [`junto_live::LiveDoc`].
//!
//! One connection, one [`tokio::select!`] loop: a socket-read arm carries
//! inbound `Update`/`Ephemeral` frames from this watcher, and an
//! `outbound`-broadcast arm carries every other frame the session produces
//! (this watcher's own accepted writes rebroadcast back to it too, same as
//! every other subscriber — importing an already-applied CRDT update a
//! second time is a no-op, not a bug). Authority for *who wrote what* is the
//! ed25519 signature this handshake authenticates and
//! [`junto_live::validate_annotation_update`] checks per annotation —
//! **never** the transport and never a claimed email inside a frame's
//! payload; see that function's module docs, and [`Connection::handle_inbound`]'s
//! `Ephemeral` arm, for why that holds for presence too.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use junto_kernel::{ChannelView, EntryId, PublicKey, Signature};
use junto_live::{Frame, validate_annotation_update};
use tokio::sync::broadcast;

use crate::host::Host;
use crate::live_plane::SessionLive;

/// How long an unauthenticated connection may hold the socket, the session's
/// `Arc<SessionLive>`, and a connection task open before sending its `Auth`
/// frame. Pre-authentication, a peer needs no credentials at all to open a
/// connection, so without a deadline it could hold all three indefinitely
/// (and repeat that any number of times) — a trivial resource-exhaustion
/// path. Generous for real network latency, short enough that it costs an
/// attacker nothing to be refused.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest websocket message this endpoint accepts, applied at the
/// upgrade (before any bytes are read). Every inbound `Update` costs a
/// `doc.fork()` plus two imports against the session's shared `LiveDoc` —
/// the same document the driving turn's loop writes into via
/// `SessionLive::publish_conversation` — so an oversized payload would tie
/// up that lock for real work, a direct breach of the live-plane's
/// never-block invariant (`crate::live_plane` module docs), and this is
/// reachable **before** authentication. Generous for a real annotation or a
/// full session snapshot, far below axum's multi-ten-MiB default.
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// `GET /channels/{channel}/sessions/{session}/live` — upgrade to the live
/// websocket protocol described in the module docs.
///
/// Channel resolution and session-id parsing happen **before** the upgrade:
/// an unresolvable channel or a malformed session id is a genuinely bad
/// request, so it gets an ordinary HTTP error response, never a socket. A
/// *resolvable* channel whose session simply isn't live right now is
/// different — that's a normal race (the watcher followed a link to a
/// session that already ended) — so it upgrades anyway and the socket itself
/// carries the graceful `Frame::End` close (see [`serve`]).
///
/// Projects via [`crate::web::project_fresh`], never the cached
/// [`crate::web::project`] every other route here uses: `authenticate`
/// below is exactly the gate a `revoke-member`/`retire-device` run in a
/// separate `junto` process exists to close, and that separate process's
/// cache invalidation on append never reaches THIS process's cache — the
/// cached path is fine for the human read surface, but not for "was this
/// key just revoked" (`junto_kernel::Ledger::project_fresh`'s doc
/// comment).
pub(crate) async fn live_session(
    State(host): State<Arc<Host>>,
    Path((channel, session)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Response {
    let (_id, view, _substrate) = match crate::web::project_fresh(&host, &channel).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let Ok(session) = session.parse::<EntryId>() else {
        return (StatusCode::BAD_REQUEST, "not a session id").into_response();
    };
    // The channel view, moved into `serve` whole (Task 10,
    // `docs/adr/0033`): the handshake now authenticates against
    // `view.keyring`'s active grants — any of a member's enrolled devices,
    // not just the first one on the Party — so `authenticate` needs both
    // `view.party` (is this email a member at all?) and `view.keyring`
    // (which of their grants are still active?) to do that and to tell a
    // failed handshake apart into one of three reasons (`authenticate`,
    // `classify_auth_failure`).
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| serve(socket, host, channel, session, view))
}

/// Everything one authenticated connection needs to handle steady-state
/// frames, bundled so [`Connection::handle_inbound`] takes the socket as its
/// only genuinely per-call argument — the rest are constants for the
/// connection's lifetime, established once in [`serve`].
struct Connection {
    host: Arc<Host>,
    channel: String,
    session: EntryId,
    live: Arc<SessionLive>,
    /// The member email this connection authenticated as (step 4) — the
    /// sole authority [`Connection::handle_inbound`] trusts for *who wrote
    /// this*, never a value carried inside a frame's own payload.
    email: String,
    keyring: HashMap<String, Vec<PublicKey>>,
}

/// One connection's whole lifetime: challenge, auth, snapshot sync, then the
/// inbound/outbound frame loop. See the module docs for the protocol and
/// [`junto_live::validate::validate_annotation_update`]'s docs for why every
/// inbound `Update` frame is checked against a fork before it ever reaches
/// `live.doc`.
async fn serve(
    mut socket: WebSocket,
    host: Arc<Host>,
    channel: String,
    session: EntryId,
    view: ChannelView,
) {
    // Step 1 (cont'd): no live document for this session — tell the watcher
    // the session is over and close, rather than erroring the upgrade (see
    // `live_session`'s doc comment on why this is graceful, not a failure).
    let Some(live) = host.live_plane().get(session) else {
        let _ = send(&mut socket, &Frame::End).await;
        return;
    };

    // Subscribe *before* exporting the snapshot below, not after: the
    // session's local-update subscription (`crate::live_plane::LivePlane::begin`)
    // forwards every commit as a `Frame::Update` the instant it happens, and
    // those updates are causally-dependent loro deltas. Subscribing after the
    // export would leave a window where a commit lands after the snapshot is
    // taken but before this receiver exists — never captured by either path,
    // stalling the watcher's document until an unrelated `Lagged` event
    // forces a resync. Subscribing first means the worst case is a commit
    // that lands in *both* the snapshot and the broadcast: importing it twice
    // is an idempotent no-op, not a bug.
    let mut outbound = live.outbound.subscribe();

    // Step 3: challenge. The nonce is an ASCII hex-shaped string, never
    // decoded — both sides sign its raw bytes (see `Frame::Challenge`'s
    // docs and the live-session-plane plan's no-hex-crate ruling).
    let nonce = random_nonce();
    if send(
        &mut socket,
        &Frame::Challenge {
            nonce: nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    // Step 4: the response must be an `Auth` frame naming a keyed member and
    // carrying a signature over the nonce's bytes that verifies against that
    // member's key. Anything else — a different frame, a socket error, an
    // unrecognized email, a bad signature, or simply never sending one — is
    // rejected and closes; there is no partial-credit path into step 5. The
    // whole exchange is bounded by `HANDSHAKE_TIMEOUT` (see its docs): an
    // unauthenticated peer gets one bounded window, not an open-ended hold on
    // this connection's socket and `Arc<SessionLive>`.
    let email = match tokio::time::timeout(HANDSHAKE_TIMEOUT, recv(&mut socket)).await {
        Ok(Some(Frame::Auth { email, signature })) => {
            match authenticate(&view, &email, &nonce, &signature) {
                Ok(()) => email,
                Err(reason) => {
                    let _ = send(&mut socket, &Frame::Rejected { reason }).await;
                    return;
                }
            }
        }
        Ok(_) => {
            let _ = send(
                &mut socket,
                &Frame::Rejected {
                    reason: "expected an Auth frame".to_string(),
                },
            )
            .await;
            return;
        }
        Err(_elapsed) => {
            let _ = send(
                &mut socket,
                &Frame::Rejected {
                    reason: "handshake timed out".to_string(),
                },
            )
            .await;
            return;
        }
    };

    // Step 5: authenticated — ack, hand over the current document as one
    // snapshot `Update`, then the current presence state, then mark this
    // watcher present themselves (and tell everyone else already watching).
    if send(&mut socket, &Frame::AuthOk).await.is_err() {
        return;
    }
    if send(&mut socket, &Frame::update(&live.doc.export_snapshot()))
        .await
        .is_err()
    {
        return;
    }
    if send(&mut socket, &Frame::ephemeral(&live.presence.encode_all()))
        .await
        .is_err()
    {
        return;
    }
    live.presence.set_watching(&email);
    let _ = live
        .outbound
        .send(Frame::ephemeral(&live.presence.encode_all()));

    // Steps 6-7: the connection's steady state. `outbound` is this session's
    // shared broadcast (`crate::live_plane::SessionLive::outbound`) — every
    // watcher, including this one, sees every accepted write.
    //
    // The keyring `Connection::handle_inbound` hands to
    // `validate_annotation_update` mirrors the handshake's own widening
    // (`docs/adr/0033`): every ACTIVE grant per email from `view.keyring`,
    // never just the Party's first-device key. A second enrolled device
    // signs its own annotations with its own key, and `sender_email` is
    // always this connection's own authenticated email — but which of
    // *that* email's devices produced the signature embedded in a given
    // annotation is a separate question from which device opened this
    // socket, so this must offer every active key, not only the one that
    // authenticated the handshake.
    let keyring: HashMap<String, Vec<PublicKey>> = view
        .keyring
        .iter()
        .map(|(email, grants)| {
            (
                email.clone(),
                grants
                    .iter()
                    .filter(|grant| grant.retired_at.is_none())
                    .map(|grant| grant.key.clone())
                    .collect(),
            )
        })
        .collect();
    let connection = Connection {
        host,
        channel,
        session,
        live,
        email,
        keyring,
    };
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(frame) = serde_json::from_str::<Frame>(&text) else {
                            // Not a frame this protocol speaks: ignored, not fatal —
                            // a forward-incompatible watcher must not be able to kill
                            // the connection with one malformed message.
                            continue;
                        };
                        if !connection.handle_inbound(&mut socket, frame).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ping/pong/binary: nothing this protocol needs.
                    Some(Err(_)) => break,
                }
            }
            frame = outbound.recv() => {
                match frame {
                    Ok(Frame::End) => {
                        let _ = send(&mut socket, &Frame::End).await;
                        break;
                    }
                    Ok(frame) => {
                        if send(&mut socket, &frame).await.is_err() {
                            break;
                        }
                    }
                    // Step 7: a slow watcher fell behind the broadcast's 256-frame
                    // buffer — resync with a fresh snapshot rather than dying; the
                    // dropped incremental updates are subsumed by it.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if send(&mut socket, &Frame::update(&connection.live.doc.export_snapshot())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

impl Connection {
    /// Handle one inbound frame from this authenticated watcher. Returns
    /// `false` when the connection should close.
    ///
    /// `Update` frames are the load-bearing case — see the module docs and
    /// [`junto_live::validate_annotation_update`]'s docs for why this
    /// method's only correct shape is "validate the fork, then import the
    /// real doc only on `Ok`", never the reverse.
    async fn handle_inbound(&self, socket: &mut WebSocket, frame: Frame) -> bool {
        match frame {
            Frame::Update { .. } => {
                let Some(bytes) = frame.update_bytes() else {
                    return self
                        .reject(socket, "update payload is not valid base64")
                        .await;
                };
                match validate_annotation_update(&self.live.doc, &bytes, &self.email, &self.keyring)
                {
                    Ok(annotations) => {
                        // Only reachable after `validate_annotation_update` returned
                        // `Ok` on a *fork* of `live.doc` — this is the one and only
                        // place untrusted bytes reach the real document.
                        match self.live.doc.import_update(&bytes) {
                            Ok(()) => {
                                let _ = self.live.outbound.send(Frame::update(&bytes));
                                if !annotations.is_empty() {
                                    // Spawned, not awaited: `deliver` does a
                                    // git shell-out (re-anchoring) plus a
                                    // ledger append (the steer note) before
                                    // it returns, and awaiting it inline here
                                    // would stall this connection's read loop
                                    // — and thus its `outbound` broadcast
                                    // receiver, capacity 256 — for exactly as
                                    // long, a direct hit on the never-block
                                    // invariant (`crate::live_plane` module
                                    // docs) landing on the *watcher*, not the
                                    // session loop.
                                    tokio::spawn(crate::live_bridge::deliver(
                                        Arc::clone(&self.host),
                                        self.channel.clone(),
                                        self.session,
                                        annotations,
                                    ));
                                }
                                true
                            }
                            // Practically unreachable: the same bytes just imported
                            // cleanly into the fork above. Still: a silent drop here
                            // would leave the sender believing an accepted-looking
                            // frame landed when it didn't, so it gets the same
                            // `Rejected` treatment as a validation failure.
                            Err(err) => {
                                self.reject(socket, &format!("failed to import: {err}"))
                                    .await
                            }
                        }
                    }
                    Err(reason) => self.reject(socket, &reason).await,
                }
            }
            Frame::Ephemeral { .. } => {
                // Presence content is never trusted, even from an authenticated
                // sender: the wire payload is a full `EphemeralStore` update that
                // could claim *any* email is watching (last-write-wins, so a
                // stale-but-fresher-timestamped claim could even displace a real
                // watcher's entry) — the one place in this protocol a client-typed
                // identity could otherwise stand in for the authenticated one. So
                // an inbound `Ephemeral` frame is treated as nothing more than a
                // heartbeat signal; the presence state actually recorded and
                // rebroadcast always comes from `self.email`, never from the
                // frame's bytes.
                self.live.presence.set_watching(&self.email);
                let _ = self
                    .live
                    .outbound
                    .send(Frame::ephemeral(&self.live.presence.encode_all()));
                true
            }
            Frame::End => false,
            // Server-only frames (`Challenge`/`AuthOk`/`Rejected`) or a second
            // `Auth` after the handshake already completed: not part of the
            // steady-state protocol, silently ignored rather than closing the
            // connection over a harmless protocol confusion.
            Frame::Challenge { .. }
            | Frame::Auth { .. }
            | Frame::AuthOk
            | Frame::Rejected { .. } => true,
        }
    }

    /// Send a `Frame::Rejected { reason }` and report whether the connection
    /// should stay open (`true`) — `false` only when the socket write itself
    /// failed, same contract as [`Connection::handle_inbound`]'s return.
    async fn reject(&self, socket: &mut WebSocket, reason: &str) -> bool {
        send(
            socket,
            &Frame::Rejected {
                reason: reason.to_string(),
            },
        )
        .await
        .is_ok()
    }
}

/// Verify a `Frame::Auth` response against the challenge nonce and every
/// active grant on `view.keyring` for the claimed email (`docs/adr/0033`):
/// any of a member's enrolled devices may sign, not just the first one
/// ever granted. `Err` carries the human-readable rejection reason,
/// classified by [`classify_auth_failure`] into one of three distinct
/// causes — the diagnosis this handshake exists to give an operator stuck
/// on "signature does not verify", the message that used to collapse a
/// stranger, an unenrolled device, and a revoked member into one dead end.
fn authenticate(
    view: &ChannelView,
    email: &str,
    nonce: &str,
    signature: &str,
) -> Result<(), String> {
    let signature =
        Signature::new(signature).map_err(|err| format!("malformed signature: {err}"))?;
    // The Party gate is checked explicitly, before the keyring is
    // consulted at all — never left to the accident that every grant in
    // `project_keyring` happens to also be a `project_party` entry today.
    // Both projections currently apply the identical `entry.author.email
    // == founder_email` guard to the identical `MemberAdded` entries
    // (`junto-kernel/src/ledger.rs`'s `project_party`/`project_keyring`),
    // so a keyring grant for a non-party email cannot be constructed
    // through the ledger today — but this handshake's accept path should
    // not *rest* on that cross-crate invariant staying true forever. This
    // check also makes the accept and [`classify_auth_failure`] paths
    // agree by construction: it is impossible to authenticate against an
    // email `classify_auth_failure` would call `NotAMember`.
    if !view.party.iter().any(|member| member.email == email) {
        return Err(render_auth_failure(AuthFailure::NotAMember, email));
    }
    let verifies = view
        .keyring
        .get(email)
        .into_iter()
        .flatten()
        .filter(|grant| grant.retired_at.is_none())
        .any(|grant| grant.key.verify_bytes(nonce.as_bytes(), &signature));
    if verifies {
        Ok(())
    } else {
        Err(render_auth_failure(
            classify_auth_failure(view, email),
            email,
        ))
    }
}

/// Why a claimed email's presented signature failed to verify against
/// every active grant on file — computed from the projected party and
/// keyring alone, and only ever consulted once [`authenticate`] has
/// already failed that check. Three genuinely different situations that a
/// bare "does not verify" cannot tell apart (`docs/adr/0033`):
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthFailure {
    /// This email never joined the channel at all — not on the Party.
    NotAMember,
    /// A member, but no active grant matches: either they never enrolled a
    /// device, or the device that just tried is not the one(s) on file.
    Unenrolled,
    /// A member with grants on record, every one of them retired — a
    /// revoked member's old device(s) trying to reconnect, not a stranger
    /// who never enrolled.
    Revoked,
}

/// Classify why `email` has no active grant that verifies, from `view`
/// alone (see [`AuthFailure`]). Membership is checked against the Party,
/// never the keyring directly: the keyring can be empty for a genuine
/// member who simply never enrolled a device, which is `Unenrolled`, not
/// `NotAMember`.
fn classify_auth_failure(view: &ChannelView, email: &str) -> AuthFailure {
    if !view.party.iter().any(|member| member.email == email) {
        return AuthFailure::NotAMember;
    }
    match view.keyring.get(email) {
        Some(grants) if !grants.is_empty() && grants.iter().all(|g| g.retired_at.is_some()) => {
            AuthFailure::Revoked
        }
        _ => AuthFailure::Unenrolled,
    }
}

/// Render an [`AuthFailure`] as the text a real human on the other end of
/// the socket reads. `email` is echoed back in every case: the caller
/// already sent it in the `Auth` frame, so echoing it back leaks nothing
/// beyond what the sender already claimed. This channel's membership by
/// email is not confidential to begin with, either: the same
/// unauthenticated router this socket is mounted on already renders every
/// party member's email into the page at `GET /channels/{channel}`
/// (`channel_page` → `render::channel_html`'s party chips, each one's
/// `title` attribute set to `escape_html(&member.email)`) — so a
/// `NotAMember` vs `Unenrolled` vs `Revoked` distinction here tells a
/// caller nothing about this channel's membership they could not already
/// read off that plain, unauthenticated page.
fn render_auth_failure(failure: AuthFailure, email: &str) -> String {
    match failure {
        AuthFailure::NotAMember => format!("'{email}' is not a member of this channel"),
        AuthFailure::Unenrolled => format!(
            "this device's key is not enrolled for '{email}' — run `junto enroll --invite …`, \
             then have the founder run `junto add-member --enroll …`"
        ),
        AuthFailure::Revoked => format!("signing access for '{email}' has been revoked"),
    }
}

/// 32 bytes of randomness rendered as an ASCII hex-shaped string — two v4
/// UUIDs' simple hex forms concatenated. Both sides of the handshake sign
/// these bytes verbatim (never a decoded byte array): see the module docs
/// and `Frame::Challenge`'s doc comment. No `hex`/`rand` dependency: `uuid`
/// is already a workspace dependency (member-code minting reuses the same
/// entropy source), and its simple hex rendering is exactly this shape.
fn random_nonce() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Serialize and send one frame, or report the socket error.
async fn send(socket: &mut WebSocket, frame: &Frame) -> Result<(), axum::Error> {
    let text = serde_json::to_string(frame).expect("Frame always serializes");
    socket.send(Message::Text(text.into())).await
}

/// Receive and parse the next frame, skipping non-text messages.
/// `None` on socket close/error or a message that never parses as a `Frame`.
async fn recv(socket: &mut WebSocket) -> Option<Frame> {
    loop {
        match socket.recv().await? {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command as StdCommand;

    use futures_util::{SinkExt, StreamExt};
    use junto_kernel::{
        Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, EntryPayload,
        LedgerEntry, Member, Span, Timestamp,
    };
    use junto_live::LiveDoc;
    use sha2::Digest as _;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    use super::*;

    type ClientSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A `CodeAnchor` annotation authored by `author_email`, unsigned — the
    /// caller signs it (or doesn't, for a negative case). Same shape as
    /// `junto_kernel::anchor::tests::sample_annotation` /
    /// `junto_live::validate::tests::test_annotation_by`.
    fn test_annotation(author_email: &str, body: &str) -> Annotation {
        Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", author_email),
            anchor: Anchor::Code(CodeAnchor {
                commit: CommitOid::new("a".repeat(40)).unwrap(),
                path: "src/live_ws.rs".into(),
                blob: ContentDigest::new("sha256:deadbeef").unwrap(),
                span: Span::new(3, 5).unwrap(),
            }),
            body: body.into(),
            excerpt: None,
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        }
    }

    /// Read frames off `ws` until one satisfies `want`, skipping (not
    /// asserting on) anything else — a connection's own accepted writes
    /// rebroadcast back to it and presence frames interleave, so a test that
    /// wants "the next `Update`" must not assume it's the very next message.
    async fn recv_until<F>(ws: &mut ClientSocket, mut want: F) -> Frame
    where
        F: FnMut(&Frame) -> bool,
    {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("timed out waiting for a frame")
                .expect("socket closed")
                .expect("websocket error");
            let ClientMessage::Text(text) = msg else {
                continue;
            };
            let frame: Frame = serde_json::from_str(&text).expect("frame parses as JSON");
            if want(&frame) {
                return frame;
            }
        }
    }

    /// `None` after up to 500ms with no frame matching `want` — used for
    /// asserting an `AuthOk` never arrives after a bad handshake. Unlike
    /// `recv_until`, a closed/errored socket is treated the same as "nothing
    /// more will arrive", not a panic: this server closes a rejected
    /// connection by dropping the socket (no `SinkExt` in non-test code, so
    /// no formal WS close frame), which a real client sees as exactly this —
    /// a reset, not a clean close.
    async fn recv_until_or_timeout<F>(ws: &mut ClientSocket, mut want: F) -> Option<Frame>
    where
        F: FnMut(&Frame) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(ClientMessage::Text(text)))) => {
                    if let Ok(frame) = serde_json::from_str::<Frame>(&text)
                        && want(&frame)
                    {
                        return Some(frame);
                    }
                }
                Ok(Some(Ok(_))) => {} // non-text: keep waiting
                Ok(Some(Err(_))) | Ok(None) => return None, // socket closed/reset
                Err(_elapsed) => return None,
            }
        }
    }

    async fn send_frame(ws: &mut ClientSocket, frame: &Frame) {
        ws.send(ClientMessage::Text(
            serde_json::to_string(frame).unwrap().into(),
        ))
        .await
        .expect("send frame");
    }

    /// A fresh test host: a temp git repo (git user `Dan <dan@x.com>`), a
    /// channel opened by that user (its genesis carries the auto-minted
    /// machine-local key — `Host::open_channel`'s `Host::keyed` call), and a
    /// live session with one conversation event so the snapshot is
    /// non-empty. Returns the host, the channel id, the session id, and the
    /// founder's signing key (the same one the party projection carries the
    /// public half of).
    async fn fixture() -> (
        Arc<Host>,
        junto_kernel::ChannelId,
        EntryId,
        junto_kernel::SigningKey,
        tempfile::TempDir,
        tempfile::TempDir,
        tokio::sync::mpsc::Receiver<crate::launch::TurnControl>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Dan"), ("user.email", "dan@x.com")] {
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
        let founder = Member::human("Dan", "dan@x.com");
        let opened = host
            .open_channel(None, "live-test", founder, None)
            .await
            .expect("open channel");
        let channel = opened.id;
        let signing_key =
            crate::keys::signing_key(member_home.path(), "dan@x.com").expect("founder signing key");

        let session = EntryId::new();
        // `LiveSessions::begin`, not `live_plane().begin` directly: this
        // also opens the session's control channel, so tests can hold
        // `control_rx` and assert an urgent annotation reaches it as a
        // `TurnControl::Steer` (`crate::live_bridge`'s delivery test).
        let control_rx = host
            .live()
            .begin(Arc::clone(&host), channel.to_string(), session, true);
        let live = host
            .live_plane()
            .get(session)
            .expect("live plane session began");
        live.doc
            .push_conversation(&serde_json::json!({"seq": 1, "kind": "status", "text": "hi"}));

        (
            host,
            channel,
            session,
            signing_key,
            dir,
            member_home,
            control_rx,
        )
    }

    /// Serve `host`'s router on an ephemeral localhost port; returns the
    /// bound address.
    async fn serve_router(host: Arc<Host>) -> std::net::SocketAddr {
        let app = crate::web::router(host);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        addr
    }

    #[tokio::test]
    async fn watcher_authenticates_receives_snapshot_and_posts_annotation() {
        let (host, channel, session, signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let addr = serve_router(host.clone()).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = signing_key.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        let auth_ok = recv_until(&mut ws, |f| {
            matches!(f, Frame::AuthOk | Frame::Rejected { .. })
        })
        .await;
        assert_eq!(
            auth_ok,
            Frame::AuthOk,
            "a correctly signed nonce authenticates"
        );

        let snapshot = recv_until(&mut ws, |f| matches!(f, Frame::Update { .. })).await;
        let local = LiveDoc::new();
        local
            .import_update(&snapshot.update_bytes().expect("update payload"))
            .expect("import snapshot");
        assert_eq!(
            local.conversation_len(),
            1,
            "the snapshot carries the one conversation event pushed before connecting"
        );

        let mut annotation = test_annotation("dan@x.com", "looks good");
        annotation.sign(&signing_key).expect("sign");
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(live) = host.live_plane().get(session)
                    && live.doc.annotations().len() == 1
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the accepted annotation lands in the real document");
        assert_eq!(
            host.live_plane().get(session).unwrap().doc.annotations()[0].body,
            "looks good"
        );

        // Negative: an unsigned annotation is rejected and never touches the
        // real document.
        let unsigned = test_annotation("dan@x.com", "unsigned, should be rejected");
        local.insert_annotation(&unsigned).expect("insert unsigned");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;
        let rejected = recv_until(&mut ws, |f| matches!(f, Frame::Rejected { .. })).await;
        let Frame::Rejected { reason } = rejected else {
            unreachable!()
        };
        assert!(reason.contains("not signed"), "unexpected reason: {reason}");
        assert_eq!(
            host.live_plane()
                .get(session)
                .unwrap()
                .doc
                .annotations()
                .len(),
            1,
            "the rejected frame must never have touched the real document"
        );
    }

    #[tokio::test]
    async fn a_signature_over_the_wrong_bytes_is_rejected_without_auth_ok() {
        let (host, channel, session, signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let addr = serve_router(host).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let _nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        // Sign different bytes than the nonce the server issued.
        let signature = signing_key.sign_bytes(b"not the nonce");
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;

        let response = recv_until(&mut ws, |f| {
            matches!(f, Frame::AuthOk | Frame::Rejected { .. })
        })
        .await;
        assert!(
            matches!(response, Frame::Rejected { .. }),
            "a signature over the wrong bytes must not authenticate: {response:?}"
        );
        assert_eq!(
            recv_until_or_timeout(&mut ws, |f| matches!(f, Frame::AuthOk)).await,
            None,
            "no AuthOk may ever follow a failed signature check"
        );
    }

    #[tokio::test]
    async fn an_unknown_email_is_rejected() {
        let (host, channel, session, signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let addr = serve_router(host).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = signing_key.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                // A real signature, but for a member absent from the channel's
                // keyring: absence must fail, never fall back to some other
                // admission path.
                email: "stranger@example.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;

        let rejected = recv_until(&mut ws, |f| matches!(f, Frame::Rejected { .. })).await;
        let Frame::Rejected { reason } = rejected else {
            unreachable!()
        };
        assert!(
            reason.contains("stranger@example.com"),
            "unexpected reason: {reason}"
        );
    }

    /// Fetch this channel's current [`ChannelView`] — the same projection
    /// `live_session` builds a handshake keyring from — for asserting on
    /// `keyring`/`party` state a test just set up.
    async fn view_of(host: &Host, channel: &junto_kernel::ChannelId) -> ChannelView {
        let crate::host::Resolution::Resolved { ledger, .. } =
            host.resolve(&channel.to_string()).await.expect("resolve")
        else {
            panic!("channel resolves");
        };
        ledger.lock().await.project(channel).await.expect("project")
    }

    /// Retire one key grant: a founder-authored `Park` targeting
    /// `grant_id`, exactly what `junto revoke-member`/`retire-device`
    /// construct (`main.rs`) and `Host::add_member`'s doc comment on
    /// re-granting after revocation describes. `at` is the park's
    /// timestamp — callers give two retirements distinct `at` values so a
    /// "some grant retired" shortcut cannot masquerade as "every grant
    /// retired".
    async fn retire_grant(
        host: &Host,
        channel: &junto_kernel::ChannelId,
        founder: &Member,
        grant_id: EntryId,
        at: Timestamp,
    ) {
        let mut park = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: *channel,
            author: founder.clone(),
            timestamp: at,
            payload: EntryPayload::Park {
                target: grant_id,
                rationale: "device lost".into(),
            },
        };
        host.sign_entry(&mut park);
        let crate::host::Resolution::Resolved { ledger, .. } =
            host.resolve(&channel.to_string()).await.expect("resolve")
        else {
            panic!("channel resolves");
        };
        ledger.lock().await.append(park).await.expect("append park");
    }

    #[tokio::test]
    async fn a_second_enrolled_device_authenticates() {
        let (host, channel, session, key_a, _dir, _member_home, _control_rx) = fixture().await;
        let key_b = junto_kernel::SigningKey::from_secret_bytes([42; 32]);
        assert_ne!(
            key_a.public_key(),
            key_b.public_key(),
            "the two devices must be genuinely distinct keypairs"
        );
        host.add_member(
            &channel.to_string(),
            &Member::human("Dan", "dan@x.com"),
            Member::human("Dan", "dan@x.com"),
            Some(key_b.public_key()),
            None,
        )
        .await
        .expect("enroll dan's second device");

        let addr = serve_router(host).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        // Signed with the SECOND device's key only. The Party projection
        // keeps carrying just the first device's key
        // (`add_member_second_device_leaves_party_row_unchanged` in
        // `host.rs`), so this can only authenticate if the handshake
        // consults `view.keyring`'s active grants, never `view.party`.
        let signature = key_b.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        let response = recv_until(&mut ws, |f| {
            matches!(f, Frame::AuthOk | Frame::Rejected { .. })
        })
        .await;
        assert_eq!(
            response,
            Frame::AuthOk,
            "a second enrolled device's key must authenticate: {response:?}"
        );
    }

    #[tokio::test]
    async fn a_second_enrolled_devices_annotation_lands_in_the_document() {
        // The socket opening on a second device's key
        // (`a_second_enrolled_device_authenticates`) is necessary but not
        // sufficient: `Connection.keyring` — what
        // `validate_annotation_update` actually checks a WRITE against —
        // is built separately from the handshake keyring, and if it were
        // still sourced from `view.party`'s single first-device key, this
        // device would authenticate the socket and then have every
        // annotation it tries to write silently rejected.
        let (host, channel, session, key_a, _dir, _member_home, _control_rx) = fixture().await;
        let key_b = junto_kernel::SigningKey::from_secret_bytes([43; 32]);
        assert_ne!(
            key_a.public_key(),
            key_b.public_key(),
            "the two devices must be genuinely distinct keypairs"
        );
        host.add_member(
            &channel.to_string(),
            &Member::human("Dan", "dan@x.com"),
            Member::human("Dan", "dan@x.com"),
            Some(key_b.public_key()),
            None,
        )
        .await
        .expect("enroll dan's second device");

        let addr = serve_router(host.clone()).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = key_b.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        assert_eq!(
            recv_until(&mut ws, |f| matches!(
                f,
                Frame::AuthOk | Frame::Rejected { .. }
            ))
            .await,
            Frame::AuthOk
        );
        recv_until(&mut ws, |f| matches!(f, Frame::Update { .. })).await; // the snapshot

        // Signed by the SECOND device's key, never the first.
        let mut annotation = test_annotation("dan@x.com", "from the second device");
        annotation.sign(&key_b).expect("sign");
        let local = LiveDoc::new();
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(live) = host.live_plane().get(session)
                    && live.doc.annotations().len() == 1
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the second device's annotation must land in the real document");
        assert_eq!(
            host.live_plane().get(session).unwrap().doc.annotations()[0].body,
            "from the second device"
        );
    }

    #[tokio::test]
    async fn an_unenrolled_device_is_told_how_to_enroll() {
        let (host, channel, session, _signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let addr = serve_router(host).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        // A real member (dan@x.com is the founder), but a device that was
        // never granted a key at all — distinct from
        // `a_signature_over_the_wrong_bytes_is_rejected_without_auth_ok`,
        // which signs the wrong bytes with the ENROLLED key; this signs
        // the right bytes with a key that has no grant whatsoever.
        let unenrolled_device = junto_kernel::SigningKey::from_secret_bytes([99; 32]);
        let signature = unenrolled_device.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;

        let rejected = recv_until(&mut ws, |f| matches!(f, Frame::Rejected { .. })).await;
        let Frame::Rejected { reason } = rejected else {
            unreachable!()
        };
        assert!(
            reason.contains("enroll"),
            "the rejection must name enrollment, not just \"does not verify\": {reason}"
        );
        assert!(
            !reason.to_lowercase().contains("revoked"),
            "an unenrolled device must never be told it was revoked: {reason}"
        );
    }

    #[tokio::test]
    async fn a_revoked_member_is_refused() {
        let (host, channel, session, _signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let founder = Member::human("Dan", "dan@x.com");
        let key_a = junto_kernel::SigningKey::from_secret_bytes([11; 32]);
        let key_b = junto_kernel::SigningKey::from_secret_bytes([22; 32]);
        host.add_member(
            &channel.to_string(),
            &founder,
            Member::human("Alice", "alice@example.com"),
            Some(key_a.public_key()),
            None,
        )
        .await
        .expect("enroll alice's first device");
        host.add_member(
            &channel.to_string(),
            &founder,
            Member::human("Alice", "alice@example.com"),
            Some(key_b.public_key()),
            None,
        )
        .await
        .expect("enroll alice's second device");

        let grant_ids: Vec<EntryId> = {
            let view = view_of(&host, &channel).await;
            let grants = view
                .keyring
                .get("alice@example.com")
                .expect("alice has grants");
            assert_eq!(
                grants.len(),
                2,
                "the fixture must give alice two distinct grants, not one, or this test could \
                 pass on a single-grant shortcut"
            );
            grants.iter().map(|g| g.granted_by).collect()
        };

        // Retire alice's FIRST device only, and prove she can still connect
        // on her SECOND (still active) device — a classifier that shortcuts
        // "revoked" to "this member's first grant is retired" (instead of
        // "every grant is") would already misreport her as revoked here,
        // before she is.
        retire_grant(
            &host,
            &channel,
            &founder,
            grant_ids[0],
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await;

        let addr = serve_router(host.clone()).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");

        // Present the RETIRED key while alice still has key_b active
        // elsewhere: since not every grant is retired yet, this must be
        // diagnosed as "wrong/unenrolled device" (case 2), never "revoked"
        // (case 3) — a classifier that only inspects the first grant on
        // the keyring (instead of requiring every grant retired) would
        // wrongly call this Revoked, because grant_ids[0] genuinely is.
        let (mut partial, _r1) = tokio_tungstenite::connect_async(url.clone())
            .await
            .expect("connect");
        let nonce = match recv_until(&mut partial, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = key_a.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut partial,
            &Frame::Auth {
                email: "alice@example.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        let rejected = recv_until(&mut partial, |f| matches!(f, Frame::Rejected { .. })).await;
        let Frame::Rejected {
            reason: partial_reason,
        } = rejected
        else {
            unreachable!()
        };
        assert!(
            !partial_reason.contains("revoked"),
            "alice still has an active device (key_b); her retired key_a must not be reported \
             as revoked yet: {partial_reason}"
        );

        // Now retire the second device too, at a further distinct
        // timestamp — only now is alice genuinely, fully revoked.
        retire_grant(
            &host,
            &channel,
            &founder,
            grant_ids[1],
            Timestamp::from_millis(1_700_000_100_000),
        )
        .await;

        let alice_grants = view_of(&host, &channel).await.keyring["alice@example.com"].clone();
        assert!(
            alice_grants.iter().all(|g| g.retired_at.is_some()),
            "both of alice's grants must be retired before the auth attempt, or this test \
             exercises nothing: {alice_grants:?}"
        );

        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = key_a.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "alice@example.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;

        let rejected = recv_until(&mut ws, |f| matches!(f, Frame::Rejected { .. })).await;
        let Frame::Rejected { reason } = rejected else {
            unreachable!()
        };
        assert!(
            reason.contains("revoked"),
            "a revoked member must be told so, not something generic: {reason}"
        );
        assert!(
            !reason.contains("enroll"),
            "telling a revoked member to enroll would be wrong and cruel: {reason}"
        );
    }

    #[tokio::test]
    async fn a_second_watcher_observes_the_first_watchers_accepted_annotation() {
        let (host, channel, session, signing_key, _dir, _member_home, _control_rx) =
            fixture().await;
        let addr = serve_router(host.clone()).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");

        async fn authenticate_and_skip_snapshot(
            ws: &mut ClientSocket,
            signing_key: &junto_kernel::SigningKey,
        ) {
            let nonce = match recv_until(ws, |f| matches!(f, Frame::Challenge { .. })).await {
                Frame::Challenge { nonce } => nonce,
                _ => unreachable!(),
            };
            let signature = signing_key.sign_bytes(nonce.as_bytes());
            send_frame(
                ws,
                &Frame::Auth {
                    email: "dan@x.com".to_string(),
                    signature: signature.into(),
                },
            )
            .await;
            assert_eq!(
                recv_until(ws, |f| matches!(f, Frame::AuthOk | Frame::Rejected { .. })).await,
                Frame::AuthOk
            );
            recv_until(ws, |f| matches!(f, Frame::Update { .. })).await; // the snapshot
        }

        let (mut writer, _r1) = tokio_tungstenite::connect_async(url.clone())
            .await
            .expect("connect writer");
        authenticate_and_skip_snapshot(&mut writer, &signing_key).await;

        let (mut watcher, _r2) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect watcher");
        authenticate_and_skip_snapshot(&mut watcher, &signing_key).await;

        let mut annotation = test_annotation("dan@x.com", "seen by the other watcher");
        annotation.sign(&signing_key).expect("sign");
        let local = LiveDoc::new();
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut writer, &Frame::update(&local.export_snapshot())).await;

        // The second connection never sent anything — it must still see the
        // rebroadcast fan-out from the first connection's accepted write.
        let fanned_out = recv_until(&mut watcher, |f| matches!(f, Frame::Update { .. })).await;
        let replay = LiveDoc::new();
        replay
            .import_update(&fanned_out.update_bytes().unwrap())
            .unwrap();
        assert_eq!(
            replay
                .annotations()
                .iter()
                .find(|a| a.id == annotation.id)
                .map(|a| a.body.clone()),
            Some("seen by the other watcher".to_string()),
            "the second watcher must receive the first watcher's accepted annotation"
        );
    }

    #[tokio::test]
    async fn urgent_annotation_steers_the_running_turn_immediately() {
        let (host, channel, session, signing_key, _dir, _member_home, mut control_rx) =
            fixture().await;
        let addr = serve_router(host).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = signing_key.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        assert_eq!(
            recv_until(&mut ws, |f| matches!(
                f,
                Frame::AuthOk | Frame::Rejected { .. }
            ))
            .await,
            Frame::AuthOk
        );
        recv_until(&mut ws, |f| matches!(f, Frame::Update { .. })).await; // the snapshot

        let mut annotation = test_annotation("dan@x.com", "off by one");
        annotation.urgent = true;
        annotation.sign(&signing_key).expect("sign");
        let local = LiveDoc::new();
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;

        let signal = tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
            .await
            .expect("a control signal arrives before the timeout")
            .expect("the control channel is still open");
        match signal {
            crate::launch::TurnControl::Steer(message) => {
                assert!(
                    message.contains("off by one"),
                    "steer message missing the annotation body: {message}"
                );
            }
            other => panic!("expected a Steer control signal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ordinary_annotation_survives_the_turn_boundary_and_is_delivered_next_begin() {
        // Finding 1 of the Task 8 fix round: a pending queue owned by
        // `SessionLive` cannot cross the turn boundary it exists to
        // cross, because `finish` drops that `SessionLive` and the next
        // `begin` recreates it empty. This must fail against that design
        // and pass once the queue lives on `LivePlane`, independent of
        // `SessionLive`'s per-turn lifecycle.
        let (host, channel, session, signing_key, _dir, _member_home, _control_rx1) =
            fixture().await;
        let addr = serve_router(host.clone()).await;

        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = signing_key.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "dan@x.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        assert_eq!(
            recv_until(&mut ws, |f| matches!(
                f,
                Frame::AuthOk | Frame::Rejected { .. }
            ))
            .await,
            Frame::AuthOk
        );
        recv_until(&mut ws, |f| matches!(f, Frame::Update { .. })).await; // the snapshot

        // Ordinary (non-urgent) — must queue, not deliver immediately.
        let mut annotation = test_annotation("dan@x.com", "please add a doc comment");
        annotation.sign(&signing_key).expect("sign");
        let local = LiveDoc::new();
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;

        // Wait for the annotation to be *queued* (not merely imported into
        // the document): `deliver` runs as a spawned task (finding 10 of
        // the Task 8 fix round), so document-import and queuing are not
        // causally ordered from this test's perspective — polling
        // `doc.annotations().len()` can observe "imported" before the
        // spawned `deliver` task has even run, which would let this test
        // race ahead to `finish`/`begin` before anything actually landed
        // in `LivePlane`'s pending queue (the state this test exists to
        // exercise). Poll `has_pending` instead — the exact state
        // `flush_pending` itself checks.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host.live_plane().has_pending(session) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the accepted annotation lands in the pending queue");

        drop(ws);

        // Simulate the turn ending, then the next turn beginning — this is
        // exactly the boundary the annotation must survive.
        host.live().finish(session);
        let mut control_rx2 = host
            .live()
            .begin(host.clone(), channel.to_string(), session, true);

        let signal = tokio::time::timeout(Duration::from_secs(5), control_rx2.recv())
            .await
            .expect("a control signal arrives before the timeout")
            .expect("the control channel is still open");
        match signal {
            crate::launch::TurnControl::Steer(message) => {
                assert!(
                    message.contains("please add a doc comment"),
                    "steer message missing the annotation body: {message}"
                );
            }
            other => panic!("expected a Steer control signal, got {other:?}"),
        }
    }

    /// Task 11 of the live-session-plane plan: the whole loop proved in one
    /// run, not just its parts in isolation. Points 1-4 are already covered
    /// by `watcher_authenticates_receives_snapshot_and_posts_annotation` and
    /// `urgent_annotation_steers_the_running_turn_immediately`, and a
    /// broadcast-fan-out variant of point 5 by
    /// `a_second_watcher_observes_the_first_watchers_accepted_annotation` —
    /// this test still exercises 1-4 itself so the six points are provably
    /// causally chained in a single session, and adds the two pieces no
    /// existing test covers: a watcher joining *after* the annotation
    /// exists (point 5, distinct from that fan-out test, which connects
    /// before the write and only ever observes it via the outbound
    /// broadcast) and `finish`'s durable archive (point 6 — `Frame::End` on
    /// every socket, plus a re-importable, ledger-anchored artifact). Each
    /// assertion below is labelled with the point of the loop it proves.
    #[tokio::test]
    async fn end_to_end_live_session_smoke() {
        // Isolates `archive_live_snapshot`'s `JUNTO_HOME`-rooted artifact
        // store from the real `~/.junto` — distinct from `fixture`'s own
        // `member_home` tempdir, which only backs signing keys.
        let home = crate::host::test_home::HomeGuard::new();
        let (host, channel, session, signing_key, dir, _member_home, mut control_rx) =
            fixture().await;
        let addr = serve_router(host.clone()).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");

        async fn authenticate(ws: &mut ClientSocket, signing_key: &junto_kernel::SigningKey) {
            let nonce = match recv_until(ws, |f| matches!(f, Frame::Challenge { .. })).await {
                Frame::Challenge { nonce } => nonce,
                _ => unreachable!(),
            };
            let signature = signing_key.sign_bytes(nonce.as_bytes());
            send_frame(
                ws,
                &Frame::Auth {
                    email: "dan@x.com".to_string(),
                    signature: signature.into(),
                },
            )
            .await;
            assert_eq!(
                recv_until(ws, |f| matches!(f, Frame::AuthOk | Frame::Rejected { .. })).await,
                Frame::AuthOk,
                "a correctly signed nonce must authenticate"
            );
        }

        // --- Points 1-2: host is up, channel + keyed member + session +
        // one conversation event exist (fixture()); a watcher connects,
        // authenticates, and imports the snapshot, seeing that event. ---
        let (mut watcher1, _r1) = tokio_tungstenite::connect_async(url.clone())
            .await
            .expect("connect watcher1");
        authenticate(&mut watcher1, &signing_key).await;
        let snapshot1 = recv_until(&mut watcher1, |f| matches!(f, Frame::Update { .. })).await;
        let local1 = LiveDoc::new();
        local1
            .import_update(&snapshot1.update_bytes().expect("update payload"))
            .expect("import snapshot");
        assert_eq!(
            local1.conversation_len(),
            1,
            "point 2: the first watcher's snapshot must carry the one conversation \
             event published before it connected"
        );

        // --- Setup for the live-fan-out property below: a second watcher
        // connects and authenticates now, strictly before any annotation
        // exists, so a later Update it receives can only be the live
        // broadcast of watcher1's write, never something already folded
        // into its own initial snapshot. Asserting `annotations().len() ==
        // 0` here is what makes that later assertion mean something. ---
        let (mut watcher2, _r2) = tokio_tungstenite::connect_async(url.clone())
            .await
            .expect("connect watcher2");
        authenticate(&mut watcher2, &signing_key).await;
        let watcher2_initial =
            recv_until(&mut watcher2, |f| matches!(f, Frame::Update { .. })).await;
        let local_w2_initial = LiveDoc::new();
        local_w2_initial
            .import_update(&watcher2_initial.update_bytes().expect("update payload"))
            .expect("import watcher2's initial snapshot");
        assert_eq!(
            local_w2_initial.annotations().len(),
            0,
            "setup: watcher2 must connect before the annotation exists — \
             otherwise the live fan-out assertion below would trivially \
             pass off its own snapshot instead of a live broadcast"
        );

        // --- Point 3: the watcher inserts a signed urgent annotation and
        // sends the update. ---
        let mut annotation = test_annotation("dan@x.com", "please double-check the bounds check");
        annotation.urgent = true;
        annotation.sign(&signing_key).expect("sign annotation");
        local1
            .insert_annotation(&annotation)
            .expect("insert locally");
        send_frame(&mut watcher1, &Frame::update(&local1.export_snapshot())).await;

        // --- Point 4a: the server document converges. ---
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(live) = host.live_plane().get(session)
                    && live.doc.annotations().len() == 1
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "point 4a: the accepted urgent annotation must land in the real \
             server document within 5s",
        );
        assert_eq!(
            host.live_plane().get(session).unwrap().doc.annotations()[0].body,
            "please double-check the bounds check",
            "point 4a: the server document must carry the exact annotation body"
        );

        // --- Point 4b: the running turn's control channel receives a Steer
        // signal carrying the annotation body. ---
        let signal = tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
            .await
            .expect("point 4b: a control signal must arrive before the timeout")
            .expect("point 4b: the control channel must still be open");
        match signal {
            crate::launch::TurnControl::Steer(message) => {
                assert!(
                    message.contains("please double-check the bounds check"),
                    "point 4b: the Steer message must carry the annotation body, got: {message}"
                );
            }
            other => panic!("point 4b: expected a Steer control signal, got {other:?}"),
        }

        // --- Live fan-out: watcher2 never sent anything itself, and (per
        // the setup assertion above) connected before the annotation
        // existed — so the next Update it receives can only be the live
        // broadcast of watcher1's accepted write. This is the property
        // point 5 names as "fan-out", proven here as it happens rather
        // than via a future snapshot (that's the separate, late-joiner
        // property proven immediately below). ---
        let fanned_out = recv_until(&mut watcher2, |f| matches!(f, Frame::Update { .. })).await;
        let local_w2_live = LiveDoc::new();
        local_w2_live
            .import_update(&fanned_out.update_bytes().expect("update payload"))
            .expect("import watcher2's live fan-out update");
        assert_eq!(
            local_w2_live
                .annotations()
                .iter()
                .find(|a| a.id == annotation.id)
                .map(|a| a.body.clone()),
            Some("please double-check the bounds check".to_string()),
            "live fan-out: a watcher connected before the write must receive \
             the accepted annotation as a live Update, not only in some \
             future snapshot"
        );

        // --- Point 5: a THIRD watcher connects only now, after the
        // annotation already exists, and must receive it in its own
        // initial snapshot — the late-joiner property, distinct from the
        // live-fan-out property just proven above. Also deliberately
        // distinct from
        // `a_second_watcher_observes_the_first_watchers_accepted_annotation`,
        // which connects before the write and only ever sees it via
        // outbound-broadcast fan-out. ---
        let (mut watcher3, _r3) = tokio_tungstenite::connect_async(url.clone())
            .await
            .expect("connect watcher3");
        authenticate(&mut watcher3, &signing_key).await;
        let snapshot3 = recv_until(&mut watcher3, |f| matches!(f, Frame::Update { .. })).await;
        let local3 = LiveDoc::new();
        local3
            .import_update(&snapshot3.update_bytes().expect("update payload"))
            .expect("import third watcher's snapshot");
        assert_eq!(
            local3.conversation_len(),
            1,
            "point 5: the third watcher's initial snapshot must still carry \
             the one conversation event"
        );
        assert_eq!(
            local3
                .annotations()
                .iter()
                .find(|a| a.id == annotation.id)
                .map(|a| a.body.clone()),
            Some("please double-check the bounds check".to_string()),
            "point 5: a watcher connecting after the annotation was accepted \
             must receive it in its own snapshot, not only via live fan-out"
        );

        // --- Point 6: finishing the session ends every socket with
        // Frame::End and archives a re-importable, ledger-anchored
        // artifact. ---
        let final_snapshot = host
            .live()
            .finish(session)
            .expect("point 6: finish must return the session's final snapshot bytes");

        // `recv_until` only ever returns a frame its predicate accepted, so
        // comparing its result to `Frame::End` can never fail — a missing
        // End would instead surface as `recv_until`'s generic "timed out" /
        // "socket closed" panic, naming neither point 6 nor which watcher.
        // `recv_until_or_timeout` makes the comparison real: `None` is a
        // reachable, distinct outcome, so each assertion below actually
        // exercises and names its own watcher.
        assert_eq!(
            recv_until_or_timeout(&mut watcher1, |f| matches!(f, Frame::End)).await,
            Some(Frame::End),
            "point 6: watcher1 must receive Frame::End when the session finishes"
        );
        assert_eq!(
            recv_until_or_timeout(&mut watcher2, |f| matches!(f, Frame::End)).await,
            Some(Frame::End),
            "point 6: watcher2 must receive Frame::End when the session finishes"
        );
        assert_eq!(
            recv_until_or_timeout(&mut watcher3, |f| matches!(f, Frame::End)).await,
            Some(Frame::End),
            "point 6: watcher3 must receive Frame::End when the session finishes"
        );

        let agent = crate::agent::Agent {
            slug: "claude".into(),
            name: "Claude".into(),
            harness: "claude".into(),
            email: "claude@junto.local".into(),
            role: None,
            model: None,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            plugins: Vec::new(),
        };
        // A literal, test-supplied name. This exercises archiving and
        // re-import, not the `turn-{turn}`-numbered collision-avoidance
        // convention itself (that convention lives in
        // `spawn_turn`/`capture_turn`, which derive the real turn number —
        // see `archive_live_snapshot`'s doc comment for why a name unique
        // per turn matters there). The literal below is both what this
        // test writes and what it reads back, so nothing here proves that
        // naming rule.
        let artifact_name = "turn-1-live.loro";
        crate::launch::archive_live_snapshot(
            &host,
            &channel.to_string(),
            channel,
            session,
            &agent,
            artifact_name,
            &final_snapshot,
        )
        .await
        .expect("point 6: archiving the finished session's snapshot must succeed");

        let archived_hex = std::fs::read_to_string(
            home.path()
                .join("artifacts")
                .join(session.to_string())
                .join(artifact_name),
        )
        .expect(
            "point 6: the archived turn-1-live.loro artifact must exist under \
             the session's artifact dir",
        );
        let archived_bytes: Vec<u8> = (0..archived_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&archived_hex[i..i + 2], 16).expect("valid hex byte"))
            .collect();
        let replay = LiveDoc::new();
        replay
            .import_update(&archived_bytes)
            .expect("point 6: the archived artifact must re-import as a valid LiveDoc update");
        assert_eq!(
            replay.conversation_len(),
            1,
            "point 6: the re-imported archive must still carry the one \
             conversation event"
        );
        assert_eq!(
            replay
                .annotations()
                .iter()
                .find(|a| a.id == annotation.id)
                .map(|a| a.body.clone()),
            Some("please double-check the bounds check".to_string()),
            "point 6: the re-imported archive must still carry the urgent annotation"
        );

        // The artifact alone is not proof it is part of the durable
        // record — the `ArtifactAttached` ledger entry is what anchors it.
        let ledger = host.ledger_for(dir.path()).await.expect("ledger_for");
        let view = ledger
            .lock()
            .await
            .project(&channel)
            .await
            .expect("project channel");
        let recorded = view
            .entries
            .iter()
            .find_map(|e| match &e.payload {
                EntryPayload::ArtifactAttached {
                    target,
                    kind,
                    provenance,
                    ..
                } if *target == session && kind == "live-snapshot" => Some(provenance.clone()),
                _ => None,
            })
            .expect(
                "point 6: an ArtifactAttached entry for this session's \
                 live-snapshot must be in the ledger",
            );
        assert_eq!(
            recorded.len(),
            1,
            "point 6: the ArtifactAttached entry must carry exactly one provenance ref"
        );
        assert!(
            recorded[0].uri.as_str().ends_with(artifact_name),
            "point 6: the recorded provenance must point at the archived \
             artifact, got: {:?}",
            recorded[0].uri
        );
        // The URI matching the filename only proves the ledger entry names
        // *a* file with the right name — not that it is the right
        // *content*. `archive_live_snapshot`'s own doc comment names
        // exactly this hazard: a colliding artifact name could silently
        // corrupt an earlier entry's recorded digest. Prove the digest
        // actually corresponds to the bytes archived on disk.
        let expected_digest = format!("sha256:{:x}", sha2::Sha256::digest(archived_hex.as_bytes()));
        assert_eq!(
            recorded[0].digest.as_ref().map(|d| d.as_str()),
            Some(expected_digest.as_str()),
            "point 6: the recorded provenance digest must match the archived \
             artifact's actual on-disk bytes, not merely share its filename"
        );
    }

    /// A fresh git repo with git user `Dan <dan@x.com>` — the founder
    /// identity `crate::invite`/`crate::add_member`/`crate::retire_device`/
    /// `crate::revoke_member` resolve via `host::git_user`, matching
    /// `fixture`'s own founder identity. Standalone (not `fixture`'s own
    /// repo helper): this test needs a REGISTERED substrate
    /// (`host::Host::from_registry`), not `fixture`'s `Host::fixed`.
    fn git_repo_dan() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .expect("git init")
                .success()
        );
        for (key, value) in [("user.name", "Dan"), ("user.email", "dan@x.com")] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        dir
    }

    /// A freshly resolved projection of `channel` under `founto_home` —
    /// deliberately a BRAND NEW `Host::from_registry` every call, never a
    /// long-held one. `Ledger::project` caches for 15s
    /// (`junto_kernel::ledger::PROJECTION_TTL`) *per Ledger instance*, and
    /// `crate::retire_device`/`crate::revoke_member`/`crate::add_member`
    /// each build their OWN `Host::from_registry` internally (mirroring
    /// separate real CLI invocations) — a long-held `Host` in this test
    /// would read its own stale cache after one of those calls appends,
    /// exactly the multi-process shape the real commands have and a single
    /// in-process `Host` does not. Reading fresh every time sidesteps that
    /// entirely, at the cost of one extra substrate read.
    async fn fresh_view(
        founder_home: &std::path::Path,
        channel: junto_kernel::ChannelId,
    ) -> ChannelView {
        let host = crate::host::Host::from_registry(founder_home.to_path_buf());
        let crate::host::Resolution::Resolved { ledger, .. } =
            host.resolve(&channel.to_string()).await.expect("resolve")
        else {
            panic!("channel resolves");
        };
        ledger
            .lock()
            .await
            .project(&channel)
            .await
            .expect("project")
    }

    /// Append `entry` through a brand-new `Host::from_registry`, for the
    /// same reason as [`fresh_view`] — a durable substrate write is correct
    /// from any instance; only a warm *read* cache is instance-local.
    async fn fresh_append(
        founder_home: &std::path::Path,
        channel: junto_kernel::ChannelId,
        entry: LedgerEntry,
    ) {
        let host = crate::host::Host::from_registry(founder_home.to_path_buf());
        let crate::host::Resolution::Resolved { ledger, .. } =
            host.resolve(&channel.to_string()).await.expect("resolve")
        else {
            panic!("channel resolves");
        };
        ledger.lock().await.append(entry).await.expect("append");
    }

    /// Drives `crate::invite`/`crate::enroll` in a genuinely separate
    /// `junto` test-binary PROCESS, `--nocapture`d, so the URL those
    /// functions print — their only output; neither has a return value —
    /// is real process stdout this call can read back with
    /// `Command::output`, rather than something an in-process `#[test]`
    /// could ever observe. `cargo test`'s default capture intercepts
    /// `print!`/`println!` via a thread-local `OUTPUT_CAPTURE` override
    /// BEFORE it reaches any OS-level stream — proven empirically while
    /// writing this test: even a `println!` from a freshly spawned
    /// `std::thread` inside the SAME process still lands inside that
    /// override (it is process-wide-once-armed, not purely per-thread —
    /// `OUTPUT_CAPTURE_USED` latches for the rest of the process once
    /// anything installs it), and redirecting the OS `stdout` handle
    /// itself (`SetStdHandle`) has no effect once `io::stdout()` has
    /// already been touched once in this process, since std caches the
    /// underlying handle rather than re-querying it — see
    /// `task-12-report.md`, finding 1. `set_output_capture` itself, which
    /// would bypass this cleanly, is `#[unstable]`/nightly-only. A
    /// genuinely separate process sidesteps all of it: its stdout is a
    /// pipe this call owns from the start, and nothing in that fresh
    /// process has touched `io::stdout()` before `--nocapture` disables
    /// the override entirely.
    ///
    /// `output.stdout` mixes libtest's own "running 1 test"/"test result"
    /// scaffolding with the target function's own `println!` line, since
    /// `--nocapture` sends both to the same real stream — the caller finds
    /// its line by the `junto://…` prefix, never by position.
    fn run_worker(mode: &str, envs: &[(&str, &str)]) -> String {
        let exe = std::env::current_exe().expect("current test exe");
        let mut cmd = StdCommand::new(exe);
        cmd.arg("live_ws::tests::end_to_end_device_enrollment_verification_and_revocation")
            .arg("--exact")
            .arg("--nocapture")
            .env("JUNTO_E2E_WORKER", mode);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let output = cmd.output().expect("spawn worker process");
        assert!(
            output.status.success(),
            "worker '{mode}' failed (status {:?})\n--- worker stdout ---\n{}\n--- worker stderr ---\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).expect("worker stdout is utf8")
    }

    /// The plan's final proof (Task 12,
    /// `.superpowers/sdd/2026-08-21-device-key-enrollment`): a scratch
    /// channel, a founder, and a real device — `invite` → `enroll` →
    /// `add-member --enroll` driven through the actual
    /// `crate::invite`/`crate::enroll`/`crate::add_member` functions
    /// (never re-implemented inline; see [`run_worker`] for how the first
    /// two hand off their printed-only output), then everything those
    /// three commands are FOR: the grant lands in the projected keyring
    /// (a), an entry the device signs verifies (b), the founder's own
    /// `keys.toml` never minted a key for the remote member (c, checked
    /// against a same-fixture positive control so the absence is not
    /// vacuous), the device's key opens a live-plane socket (d) AND can
    /// actually WRITE through it (g — the exact half Task 10 shipped
    /// broken, `a_second_enrolled_devices_annotation_lands_in_the_document`
    /// pinned in isolation; this proves it with a key that arrived through
    /// the real enrollment flow) — and finally revocation, extended past
    /// the brief's own (e)/(f) into the operator sequence that motivated
    /// `7b27008` (h): retiring ONE of alice's two devices must not offboard
    /// her (only the LATEST retirement across every device does), and once
    /// every device is retired, everything after that latest cutoff stops
    /// counting while everything at-or-before it, on ANY of her devices,
    /// still does.
    #[tokio::test]
    async fn end_to_end_device_enrollment_verification_and_revocation() {
        // ---- worker mode: see `run_worker`'s doc comment. Re-entered by
        // `run_worker` as a fresh process with `JUNTO_E2E_WORKER` set; a
        // normal `cargo test` run never sets it, so normal discovery of
        // this same test name takes the orchestrator branch below.
        if let Ok(mode) = std::env::var("JUNTO_E2E_WORKER") {
            match mode.as_str() {
                "invite" => {
                    let channel = std::env::var("JUNTO_E2E_CHANNEL").expect("channel env");
                    let member = std::env::var("JUNTO_E2E_MEMBER").expect("member env");
                    crate::invite(channel, member)
                        .await
                        .expect("worker: invite");
                }
                "enroll" => {
                    let invite_url = std::env::var("JUNTO_E2E_INVITE_URL").expect("invite url env");
                    let name = std::env::var("JUNTO_E2E_NAME").expect("name env");
                    crate::enroll(invite_url, Some(name))
                        .await
                        .expect("worker: enroll");
                }
                other => panic!("unknown JUNTO_E2E_WORKER mode '{other}'"),
            }
            return;
        }

        // ---- orchestrator: "two junto homes on one machine" — the
        // founder's real ~/.junto stand-in, and alice's, genuinely
        // separate directories the invite/enroll legs run against as
        // separate processes (see `run_worker`).
        let _home = crate::host::test_home::HomeGuard::new();
        let founder_home = _home.path().to_path_buf();
        let repo = git_repo_dan();
        crate::host::register_substrate(&founder_home, repo.path()).expect("register substrate");
        let bootstrap = crate::host::Host::from_registry(founder_home.clone());
        let opened = bootstrap
            .open_channel(
                Some(repo.path()),
                "scratch",
                Member::human("Dan", "dan@x.com"),
                None,
            )
            .await
            .expect("open channel");
        let channel = opened.id;
        let alice_home = tempfile::tempdir().expect("alice's own machine's home");

        // ---- invite → enroll, each through the real CLI function ----
        let invite_stdout = run_worker(
            "invite",
            &[
                ("JUNTO_HOME", founder_home.to_str().expect("utf8 path")),
                ("JUNTO_E2E_CHANNEL", &channel.to_string()),
                ("JUNTO_E2E_MEMBER", "alice@example.com"),
            ],
        );
        let invite_url = invite_stdout
            .lines()
            .find(|line| line.starts_with("junto://invite?code="))
            .unwrap_or_else(|| panic!("no invite URL in worker output:\n{invite_stdout}"))
            .to_string();

        let enroll_stdout = run_worker(
            "enroll",
            &[
                ("JUNTO_HOME", alice_home.path().to_str().expect("utf8 path")),
                ("JUNTO_E2E_INVITE_URL", &invite_url),
                ("JUNTO_E2E_NAME", "Alice's Laptop"),
            ],
        );
        let enroll_url = enroll_stdout
            .lines()
            .find(|line| line.starts_with("junto://enroll?code="))
            .unwrap_or_else(|| panic!("no enroll URL in worker output:\n{enroll_stdout}"))
            .to_string();

        // ---- add-member --enroll: real function, in-process — its
        // OBSERVABLE side effect is the ledger append, not its println,
        // so no worker process is needed for this leg.
        crate::add_member(
            channel.to_string(),
            None,
            None,
            Some("human".to_string()),
            None, // --author-name/--author-email: default to git_user(&substrate)
            None,
            None,
            Some(enroll_url),
        )
        .await
        .expect("add-member --enroll");

        // Alice's device minted its OWN key on ITS OWN home —
        // `keys::signing_key` reuses on a second call, so this reads back
        // the exact key the real `enroll()` worker process minted, never a
        // fresh one.
        let alice_key_a = crate::keys::signing_key(alice_home.path(), "alice@example.com")
            .expect("alice's device key");

        // (a) the new grant appears in the projected keyring. Fails if
        // add_member's --enroll path never appended MemberAdded, appended
        // a re-minted key instead of the device's own, or appended it
        // already retired.
        let view = fresh_view(&founder_home, channel).await;
        let grant_a = view
            .keyring
            .get("alice@example.com")
            .and_then(|grants| grants.iter().find(|g| g.key == alice_key_a.public_key()))
            .expect("alice's device key is on the projected keyring")
            .clone();
        assert!(
            grant_a.retired_at.is_none(),
            "freshly granted, must be active"
        );

        // (c) the founder's own keys.toml holds no key for alice. The real
        // counterfactual for this check is the plan's original bug: a
        // pre-fix `Host::keyed` (host.rs:371-377) that minted locally even
        // when a device's own key WAS supplied — the negative assertion
        // below guards exactly that, and the `dan@x.com` sanity check
        // right above already proves `has_signing_key` sees this exact
        // store. The control that follows is a narrower, same-fixture
        // positive: a KEYLESS *agent* grant (not human — a keyless HUMAN
        // is refused outright, `host.rs:665-678`, never reaching
        // `Host::keyed` at all) on this identical host DOES mint locally,
        // ruling out "this store/check silently never works in this
        // fixture" as an alternative explanation for the absence.
        assert!(
            crate::keys::has_signing_key(&founder_home, "dan@x.com").expect("check"),
            "sanity: the founder's own key really is on this machine"
        );
        assert!(
            !crate::keys::has_signing_key(&founder_home, "alice@example.com").expect("check"),
            "an enrolled member's key must come from their own device, never be minted here"
        );
        bootstrap
            .add_member(
                &channel.to_string(),
                &Member::human("Dan", "dan@x.com"),
                Member::agent("Worker", "worker@agents.junto"),
                None,
                None,
            )
            .await
            .expect("keyless control grant");
        assert!(
            crate::keys::has_signing_key(&founder_home, "worker@agents.junto").expect("check"),
            "control: a keyless AGENT grant on this SAME host DOES mint locally — rules out a \
             broken has_signing_key/keys.toml in this fixture (the counterfactual (c) actually \
             guards against is Host::keyed ignoring a supplied key, not this control)"
        );

        // (b) an entry signed by the new device's key projects as
        // verified. Fails if the recorded grant's key does not match what
        // alice's device actually holds, or if `project_unverified` stops
        // consulting the keyring correctly.
        let mut verified_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: Member::human("Alice", "alice@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Assertion {
                statement: "the migration lands cleanly".into(),
                rationale: "ran it twice".into(),
                provenance: Vec::new(),
                frame: None,
            },
        };
        verified_entry
            .sign(&alice_key_a)
            .expect("sign with alice's real device key");
        let verified_entry_id = verified_entry.id;
        fresh_append(&founder_home, channel, verified_entry).await;
        let view = fresh_view(&founder_home, channel).await;
        assert!(
            !view.unverified.contains(&verified_entry_id),
            "an entry genuinely signed by the enrolled device's key must verify"
        );
        assert!(!view.unrecognized.contains(&verified_entry_id));

        // ---- live session: one persistent Arc<Host>, used only for the
        // in-memory live plane, never touched again for ledger reads
        // afterward (see `fresh_view`'s doc comment on why).
        let live_host = crate::host::Host::from_registry(founder_home.clone());
        let session = EntryId::new();
        let _control_rx =
            live_host
                .live()
                .begin(Arc::clone(&live_host), channel.to_string(), session, true);
        let live = live_host
            .live_plane()
            .get(session)
            .expect("live session began");
        live.doc
            .push_conversation(&serde_json::json!({"seq": 1, "kind": "status", "text": "hi"}));

        let addr = serve_router(Arc::clone(&live_host)).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("connect");
        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = alice_key_a.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "alice@example.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;

        // (d) the live-plane handshake succeeds with the enrolled
        // device's key.
        let response = recv_until(&mut ws, |f| {
            matches!(f, Frame::AuthOk | Frame::Rejected { .. })
        })
        .await;
        assert_eq!(
            response,
            Frame::AuthOk,
            "the enrolled device's key must authenticate: {response:?}"
        );
        recv_until(&mut ws, |f| matches!(f, Frame::Update { .. })).await; // the snapshot

        // (g) — and can actually WRITE: Task 10's silent half-wiring bug.
        // Signed by the SAME key that just authenticated the socket, but
        // checked against `Connection.keyring`, built separately — if that
        // were still sourced from the Party's single first-device map
        // instead of the real keyring, this write would be silently
        // dropped even though the socket opened cleanly.
        let mut annotation =
            test_annotation("alice@example.com", "from alice's real enrolled device");
        annotation.sign(&alice_key_a).expect("sign annotation");
        let local = LiveDoc::new();
        local.insert_annotation(&annotation).expect("insert");
        send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(live) = live_host.live_plane().get(session)
                    && live.doc.annotations().len() == 1
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the enrolled device's annotation must land in the real document");
        assert_eq!(
            live_host
                .live_plane()
                .get(session)
                .unwrap()
                .doc
                .annotations()[0]
                .body,
            "from alice's real enrolled device"
        );

        // ---- revocation: a second device for alice (granted directly —
        // the enrollment protocol itself is already fully proven above by
        // device A; this device exists only so the kernel's cutoff fold
        // has two grants to distinguish "one retired" from "all retired"),
        // then the LATEST-retirement operator sequence that motivated
        // `7b27008` (h).
        let key_b = junto_kernel::SigningKey::from_secret_bytes([91; 32]);
        assert_ne!(
            alice_key_a.public_key(),
            key_b.public_key(),
            "alice's two devices must be genuinely distinct keypairs"
        );
        bootstrap
            .add_member(
                &channel.to_string(),
                &Member::human("Dan", "dan@x.com"),
                Member::human("Alice", "alice@example.com"),
                Some(key_b.public_key()),
                None,
            )
            .await
            .expect("grant alice's second device");

        // step 1: retire-device A at T1 — through the real command.
        crate::retire_device(
            channel.to_string(),
            grant_a.granted_by.to_string(),
            "device A lost".to_string(),
        )
        .await
        .expect("retire-device");
        // `retire_device`/`revoke_member` stamp `Timestamp::now()`
        // internally (no way to inject a value) — sleep between each real
        // wall-clock-stamped step so T1, the mid entry, and T2 land at
        // genuinely distinct milliseconds, never collapsing into a
        // same-instant race that would make "genuinely distinct" false by
        // accident.
        tokio::time::sleep(Duration::from_millis(30)).await;

        // step 2: an entry from device B, stamped after T1.
        let mut mid_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: Member::human("Alice", "alice@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Assertion {
                statement: "still working from my other laptop".into(),
                rationale: "device A is just lost, not me".into(),
                provenance: Vec::new(),
                frame: None,
            },
        };
        mid_entry.sign(&key_b).expect("sign with device B");
        let mid_entry_id = mid_entry.id;
        fresh_append(&founder_home, channel, mid_entry).await;

        let view = fresh_view(&founder_home, channel).await;
        assert!(
            !view.unrecognized.contains(&mid_entry_id),
            "partial retirement (device A only) must not offboard alice: {:?}",
            view.unrecognized
        );
        assert!(
            view.standings.contains_key(&mid_entry_id),
            "a recognized assertion must carry a standing"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;

        // step 3: revoke-member at T2 — parks every remaining active
        // grant (device B) — through the real command.
        crate::revoke_member(
            channel.to_string(),
            "alice@example.com".to_string(),
            "left the project".to_string(),
        )
        .await
        .expect("revoke-member");
        tokio::time::sleep(Duration::from_millis(30)).await;

        // step 4: an entry after T2.
        let mut after_entry = LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel,
            author: Member::human("Alice", "alice@example.com"),
            timestamp: Timestamp::now(),
            payload: EntryPayload::Assertion {
                statement: "one more, after leaving".into(),
                rationale: "should not count".into(),
                provenance: Vec::new(),
                frame: None,
            },
        };
        after_entry.sign(&key_b).expect("sign");
        let after_entry_id = after_entry.id;
        fresh_append(&founder_home, channel, after_entry).await;

        let view = fresh_view(&founder_home, channel).await;
        // Precondition for everything below: T1 < mid entry < T2,
        // STRICTLY and GENUINELY distinct — never assumed from the two
        // 30ms sleeps alone. `retire_device`/`revoke_member` stamp
        // `Timestamp::now()` internally (no way to inject a value), and
        // `Timestamp::now()` is millisecond-resolution over a
        // non-monotonic `SystemTime` with ~15.6ms tick granularity on
        // Windows — two ticks of sleep is not a proof. If T1 and T2 ever
        // landed on the same tick (or the clock stepped back), `min ==
        // max` and the crux assertion below would pass whether the fold
        // takes the earliest or the latest retirement — the exact
        // regression this test exists to catch, silently disarmed. Assert
        // the bracket explicitly, from the SAME final projection the
        // criteria below read, so a collapsed window fails loudly here
        // instead of quietly validating nothing.
        let alice_grants = view
            .keyring
            .get("alice@example.com")
            .expect("alice still has grants on record");
        let t1 = alice_grants
            .iter()
            .find(|g| g.granted_by == grant_a.granted_by)
            .and_then(|g| g.retired_at)
            .expect("device A's grant must be retired (step 1)");
        let t2 = alice_grants
            .iter()
            .find(|g| g.key == key_b.public_key())
            .and_then(|g| g.retired_at)
            .expect("device B's grant must be retired (step 3)");
        let mid_ts = view
            .entries
            .iter()
            .find(|e| e.id == mid_entry_id)
            .expect("the mid entry is on record")
            .timestamp;
        assert!(
            t1 < mid_ts,
            "the mid entry must be stamped strictly after T1 (device A's retirement): \
             T1={t1:?} mid={mid_ts:?}"
        );
        assert!(
            mid_ts < t2,
            "the mid entry must be stamped strictly before T2 (device B's retirement): \
             mid={mid_ts:?} T2={t2:?}"
        );
        assert_ne!(t1, t2, "T1 and T2 must be genuinely distinct retirements");

        // (e): a later entry is unrecognized once every device is retired.
        assert!(
            view.unrecognized.contains(&after_entry_id),
            "an entry after full revocation must be unrecognized"
        );
        assert!(!view.standings.contains_key(&after_entry_id));

        // (h)'s crux: the T1..T2 entry from device B (step 2) must STILL
        // count after the full sequence — the exact assertion that would
        // have failed before `7b27008`'s earliest-vs-latest fix, when
        // retiring device A alone would have retroactively unrecognized
        // this once device B was later retired too.
        assert!(
            !view.unrecognized.contains(&mid_entry_id),
            "the T1..T2 entry from device B must still count after full revocation"
        );
        assert!(view.standings.contains_key(&mid_entry_id));

        // (f): the earlier (pre-retirement, criterion-b) entry keeps its
        // standing — revocation must never retroactively unrecognize an
        // entry written before ANY cutoff existed. This entry sits
        // strictly before both T1 and T2, so it is the one a "retirement
        // applies too broadly" regression (e.g. an unbounded/backward
        // cutoff, or folding starting from the wrong end of the grant
        // list) would catch — not a boundary `>=`-vs-`>` flip at the
        // cutoff itself (`project_unrecognized`'s `entry.timestamp >
        // cutoff`), which only reclassifies an entry stamped EXACTLY AT a
        // cutoff, and this one is not.
        assert!(
            !view.unrecognized.contains(&verified_entry_id),
            "revocation must never retroactively unrecognize entries written before any cutoff"
        );
        assert!(view.standings.contains_key(&verified_entry_id));

        // Revocation must never remove alice from the party (`docs/adr/0035`).
        assert!(
            view.party.iter().any(|m| m.email == "alice@example.com"),
            "a fully revoked member must stay in the party"
        );
    }

    /// Regression guard for the staleness `live_session` used to have: a
    /// long-running `junto serve` (`host` here, held open across the whole
    /// connection, exactly like a real server) and a `junto revoke-member`
    /// run in a genuinely SEPARATE process (`revoker` — an independent
    /// `Host::fixed_with_member_home` over the identical substrate, so an
    /// independent `Ledger` with its own in-memory projection cache) race:
    /// `revoker`'s own cache invalidation on append never reaches `host`'s
    /// cache at all. Before `live_session` switched to
    /// `crate::web::project_fresh`, a `host` whose cache was already warm
    /// (any earlier request for this channel) would keep authenticating
    /// alice's now-revoked key for up to `PROJECTION_TTL` (15s) after
    /// `revoker`'s revocation already returned — this test forces exactly
    /// that warm-cache condition (an explicit `view_of(&host, ..)` call
    /// before the revocation) so a regression back to the cached
    /// `crate::web::project` fails this test immediately rather than only
    /// intermittently, depending on whether some earlier request happened
    /// to warm the cache first.
    #[tokio::test]
    async fn a_revocation_from_a_separate_process_is_visible_to_a_live_connection_immediately() {
        let (host, channel, session, _key_a, dir, member_home, _control_rx) = fixture().await;
        let founder = Member::human("Dan", "dan@x.com");
        let alice_key = junto_kernel::SigningKey::from_secret_bytes([55; 32]);
        host.add_member(
            &channel.to_string(),
            &founder,
            Member::human("Alice", "alice@example.com"),
            Some(alice_key.public_key()),
            None,
        )
        .await
        .expect("grant alice a device");

        // Warm `host`'s own projection cache — the state any real
        // `junto serve` would already be in after serving one earlier
        // request for this channel (the channel page, an earlier live
        // connection, anything).
        let view_before = view_of(&host, &channel).await;
        assert!(
            view_before
                .keyring
                .get("alice@example.com")
                .is_some_and(|grants| grants.iter().any(|g| g.retired_at.is_none())),
            "sanity: alice's grant must be active (and now cached) before the revocation"
        );

        // A SEPARATE `Host` over the identical substrate + member home —
        // a real separate `junto revoke-member` process would build
        // exactly this, never `host`'s in-memory cache.
        let revoker = Host::fixed_with_member_home(
            vec![dir.path().to_path_buf()],
            Some(member_home.path().to_path_buf()),
        );
        let grant_id = view_of(&revoker, &channel)
            .await
            .keyring
            .get("alice@example.com")
            .and_then(|grants| grants.iter().find(|g| g.key == alice_key.public_key()))
            .expect("alice's grant, read fresh by the revoking process")
            .granted_by;
        retire_grant(&revoker, &channel, &founder, grant_id, Timestamp::now()).await;

        // Connect through the ORIGINAL, warm-cached `host` and try to
        // authenticate with alice's now-revoked key.
        let addr = serve_router(host.clone()).await;
        let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
        let (mut ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");
        let nonce = match recv_until(&mut ws, |f| matches!(f, Frame::Challenge { .. })).await {
            Frame::Challenge { nonce } => nonce,
            _ => unreachable!(),
        };
        let signature = alice_key.sign_bytes(nonce.as_bytes());
        send_frame(
            &mut ws,
            &Frame::Auth {
                email: "alice@example.com".to_string(),
                signature: signature.into(),
            },
        )
        .await;
        let response = recv_until(&mut ws, |f| {
            matches!(f, Frame::AuthOk | Frame::Rejected { .. })
        })
        .await;
        assert!(
            matches!(response, Frame::Rejected { .. }),
            "a revocation from a separate process must be visible to this connection \
             immediately, not after PROJECTION_TTL: {response:?}"
        );
    }
}

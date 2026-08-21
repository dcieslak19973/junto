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
//! payload; see that function's module docs for why.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use junto_kernel::{EntryId, PublicKey, Signature};
use junto_live::{Frame, validate_annotation_update};
use tokio::sync::broadcast;

use crate::host::Host;

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
pub(crate) async fn live_session(
    State(host): State<Arc<Host>>,
    Path((channel, session)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Response {
    let (_id, view, _substrate) = match crate::web::project(&host, &channel).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let Ok(session) = session.parse::<EntryId>() else {
        return (StatusCode::BAD_REQUEST, "not a session id").into_response();
    };
    // The channel keyring (`docs/adr/0033`): every member who can be
    // authenticated is one who was granted a key on a membership-granting
    // entry (genesis or `MemberAdded`) — mirrors the party-projection
    // keyring build in `junto_kernel::ledger::project_unverified`. Members
    // without a key are simply absent, not admitted some other way.
    let keyring: HashMap<String, PublicKey> = view
        .party
        .iter()
        .filter_map(|member| {
            member
                .public_key
                .clone()
                .map(|key| (member.email.clone(), key))
        })
        .collect();
    ws.on_upgrade(move |socket| serve(socket, host, channel, session, keyring))
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
    keyring: HashMap<String, PublicKey>,
) {
    // Step 1 (cont'd): no live document for this session — tell the watcher
    // the session is over and close, rather than erroring the upgrade (see
    // `live_session`'s doc comment on why this is graceful, not a failure).
    let Some(live) = host.live_plane().get(session) else {
        let _ = send(&mut socket, &Frame::End).await;
        return;
    };

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
    // unrecognized email, a bad signature — rejects and closes; there is no
    // partial-credit path into step 5.
    let email = match recv(&mut socket).await {
        Some(Frame::Auth { email, signature }) => {
            match authenticate(&keyring, &email, &nonce, &signature) {
                Ok(()) => email,
                Err(reason) => {
                    let _ = send(&mut socket, &Frame::Rejected { reason }).await;
                    return;
                }
            }
        }
        _ => {
            let _ = send(
                &mut socket,
                &Frame::Rejected {
                    reason: "expected an Auth frame".to_string(),
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
    let mut outbound = live.outbound.subscribe();
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
                        if !handle_inbound(frame, &mut socket, &host, &channel, session, &live, &email, &keyring).await {
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
                        if send(&mut socket, &Frame::update(&live.doc.export_snapshot())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Handle one inbound frame from an authenticated watcher. Returns `false`
/// when the connection should close.
///
/// `Update` frames are the load-bearing case — see the module docs and
/// [`junto_live::validate_annotation_update`]'s docs for why this function's
/// only correct shape is "validate the fork, then import the real doc only
/// on `Ok`", never the reverse.
#[allow(clippy::too_many_arguments)]
async fn handle_inbound(
    frame: Frame,
    socket: &mut WebSocket,
    host: &Arc<Host>,
    channel: &str,
    session: EntryId,
    live: &crate::live_plane::SessionLive,
    email: &str,
    keyring: &HashMap<String, PublicKey>,
) -> bool {
    match frame {
        Frame::Update { .. } => {
            let Some(bytes) = frame.update_bytes() else {
                return true;
            };
            match validate_annotation_update(&live.doc, &bytes, email, keyring) {
                Ok(annotations) => {
                    // Only reachable after `validate_annotation_update` returned
                    // `Ok` on a *fork* of `live.doc` — this is the one and only
                    // place untrusted bytes reach the real document.
                    if live.doc.import_update(&bytes).is_ok() {
                        let _ = live.outbound.send(Frame::update(&bytes));
                        if !annotations.is_empty() {
                            crate::live_bridge::deliver(
                                Arc::clone(host),
                                channel.to_string(),
                                session,
                                annotations,
                            )
                            .await;
                        }
                    }
                }
                Err(reason) => {
                    if send(socket, &Frame::Rejected { reason }).await.is_err() {
                        return false;
                    }
                }
            }
            true
        }
        Frame::Ephemeral { .. } => {
            if let Some(bytes) = frame.ephemeral_bytes() {
                // Presence is last-write-wins and untrusted-but-harmless (it can
                // only ever claim "such-and-such email is watching"), so it is
                // applied and rebroadcast without going through the annotation
                // validation gate — that gate exists for `live.doc`, not presence.
                if live.presence.apply(&bytes).is_ok() {
                    let _ = live.outbound.send(Frame::ephemeral(&bytes));
                }
            }
            true
        }
        Frame::End => false,
        // Server-only frames (`Challenge`/`AuthOk`/`Rejected`) or a second
        // `Auth` after the handshake already completed: not part of the
        // steady-state protocol, silently ignored rather than closing the
        // connection over a harmless protocol confusion.
        Frame::Challenge { .. } | Frame::Auth { .. } | Frame::AuthOk | Frame::Rejected { .. } => {
            true
        }
    }
}

/// Verify a `Frame::Auth` response against the challenge nonce and the
/// channel keyring. `Err` carries the human-readable rejection reason.
fn authenticate(
    keyring: &HashMap<String, PublicKey>,
    email: &str,
    nonce: &str,
    signature: &str,
) -> Result<(), String> {
    let key = keyring
        .get(email)
        .ok_or_else(|| format!("no verifying key on file for '{email}'"))?;
    let signature =
        Signature::new(signature).map_err(|err| format!("malformed signature: {err}"))?;
    if key.verify_bytes(nonce.as_bytes(), &signature) {
        Ok(())
    } else {
        Err("signature does not verify against the key on file".to_string())
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

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
use junto_kernel::{EntryId, PublicKey, Signature};
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
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| serve(socket, host, channel, session, keyring))
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
    keyring: HashMap<String, PublicKey>,
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
            match authenticate(&keyring, &email, &nonce, &signature) {
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
                                    crate::live_bridge::deliver(
                                        Arc::clone(&self.host),
                                        self.channel.clone(),
                                        self.session,
                                        annotations,
                                    )
                                    .await;
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

#[cfg(test)]
mod tests {
    use std::process::Command as StdCommand;

    use futures_util::{SinkExt, StreamExt};
    use junto_kernel::{
        Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, Member, Span,
        Timestamp,
    };
    use junto_live::LiveDoc;
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
        let control_rx = host.live().begin(session);
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
}

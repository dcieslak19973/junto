//! Integration test for the authenticated live websocket endpoint
//! (`docs/superpowers/specs/2026-08-20-live-session-plane-design.md` Task 7):
//! a real `tokio-tungstenite` client against a real `axum::serve` router,
//! driving the whole challenge-response → snapshot-sync → validated-write
//! protocol end to end. Lives in `tests/` (not an inline `#[cfg(test)] mod`)
//! because it needs a genuine `TcpListener` and a second crate on the wire —
//! see `crates/junto/src/lib.rs`'s module doc for why the crate has a lib
//! target at all.

use std::process::Command as StdCommand;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use junto::host::Host;
use junto::web;
use junto_kernel::{
    Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, EntryId, Member, Span,
    Timestamp,
};
use junto_live::{Frame, LiveDoc};
use tokio_tungstenite::tungstenite::Message;

/// A `CodeAnchor` annotation authored by `author`, unsigned — the caller
/// signs it (or doesn't, for the negative case). Same shape as
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

/// Read frames off the client socket until one satisfies `want`, skipping
/// (not asserting on) anything else — this connection's own accepted writes
/// rebroadcast back to it and presence frames interleave, so a test that
/// wants "the next `Update`" must not assume it's the very next message.
async fn recv_until<F>(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut want: F,
) -> Frame
where
    F: FnMut(&Frame) -> bool,
{
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("socket closed")
            .expect("websocket error");
        let Message::Text(text) = msg else { continue };
        let frame: Frame = serde_json::from_str(&text).expect("frame parses as JSON");
        if want(&frame) {
            return frame;
        }
    }
}

async fn send_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    frame: &Frame,
) {
    ws.send(Message::Text(serde_json::to_string(frame).unwrap()))
        .await
        .expect("send frame");
}

#[tokio::test]
async fn watcher_authenticates_receives_snapshot_and_posts_annotation() {
    // 1. tempdir repo + git init; Host::fixed_with_member_home.
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

    // 2. Open a channel whose genesis member carries a public key: `open_channel`
    // attaches the founder's machine-local key (`Host::keyed`) unconditionally, so
    // no explicit `Member::with_key` is needed here — the same key this test signs
    // with (`junto::keys::signing_key`) is exactly the one the party projection
    // will carry.
    let founder = Member::human("Dan", "dan@x.com");
    let opened = host
        .open_channel(None, "live-test", founder, None)
        .await
        .expect("open channel");
    let channel = opened.id;
    let signing_key =
        junto::keys::signing_key(member_home.path(), "dan@x.com").expect("founder signing key");

    // 3. A live session with one conversation event, so the snapshot is non-empty.
    let session = EntryId::new();
    let live = host.live_plane().begin(session);
    live.doc
        .push_conversation(&serde_json::json!({"seq": 1, "kind": "status", "text": "hi"}));

    // 4. Serve the real router on an ephemeral port.
    let app = web::router(host.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // 5. Connect.
    let url = format!("ws://{addr}/channels/{channel}/sessions/{session}/live");
    let (mut ws, _response) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");

    // 6. Challenge → sign → Auth → AuthOk → snapshot Update.
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

    // 7. A signed annotation, sent as a snapshot update from a local fork.
    let mut annotation = test_annotation("dan@x.com", "looks good");
    annotation.sign(&signing_key).expect("sign");
    local.insert_annotation(&annotation).expect("insert");
    send_frame(&mut ws, &Frame::update(&local.export_snapshot())).await;

    // 8. The server's real document gains it — poll with a timeout.
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

    // 9. Negative: an unsigned annotation is rejected and never touches the
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

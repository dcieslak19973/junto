//! Fork-validated annotation import — the gate between a remote watcher's
//! bytes and the session's real [`LiveDoc`].
//!
//! # Why this validates a fork, never the real document
//!
//! A loro import merges unconditionally: there is no "import, then roll
//! back if it turns out to be bad". By the time an import call returns, its
//! CRDT ops are woven into the document's history — permanently, and
//! (because loro is a CRDT) in a way later ops can build on. So checking the
//! *real* document's contents after importing untrusted bytes is already too
//! late: an unsigned or forged annotation would already be live, merged into
//! every watcher's copy on the next sync, before this function ever noticed.
//!
//! [`validate_annotation_update`] therefore never imports into the document
//! it is given. It forks the document (an independent, cheap, in-memory
//! copy — see [`LiveDoc::fork`]), imports the untrusted bytes into *that*
//! fork, and inspects what appeared there. The real document is touched only
//! by the caller, and only after this function returns `Ok`. **Do not
//! "simplify" this into import-then-check on the real `doc`** — that
//! refactor looks harmless and silently destroys the one property this
//! module exists to guarantee. `rejected_frame_leaves_server_document_unchanged`
//! below is not incidental test coverage; it is the proof that the guarantee
//! holds.

use std::collections::HashMap;

use junto_kernel::{Annotation, PublicKey};

use crate::LiveDoc;

/// Validate an incoming `Update` frame's payload before it is allowed into
/// `doc`.
///
/// `doc` is the session's real, live document — **read-only** as far as
/// this function is concerned; see the module docs for why. `bytes` is the
/// (already base64-decoded) loro update/snapshot payload from the frame.
/// `sender_email` is the already-authenticated member the bytes claim to
/// come from (established by the `Challenge`/`Auth` handshake, not by
/// anything inside `bytes`). `keyring` maps every known member's email to
/// their [`PublicKey`], the record of authority this function checks
/// signatures against.
///
/// A remote watcher may only ever *add* annotations. Every check below is
/// therefore scoped to annotations that are new in the fork relative to
/// `doc` — an update that only rewrites already-converged state (the normal
/// case for loro's CRDT merge semantics) needs no re-validation of what was
/// already accepted.
///
/// Three checks apply to each new annotation, all of which must pass for
/// the *entire* frame to be accepted:
/// 1. it parses as an [`Annotation`] at all;
/// 2. its `author.email` equals `sender_email` — a member may not post an
///    annotation attributed to someone else;
/// 3. it carries a signature that verifies against `keyring[sender_email]`.
///
/// One structural check applies to the frame as a whole: a watcher may
/// write only into the `annotations` container. If the fork's
/// `conversation_len` or `worktree_len` differs from `doc`'s, the frame
/// touched a driver-only container and is rejected outright, regardless of
/// what else it contains.
///
/// Any single failure rejects the **whole frame** — there is no partial
/// acceptance. A frame that adds no annotations and does not touch
/// `conversation`/`worktree` is legitimate (e.g. it may carry loro metadata
/// with no visible new keys) and returns `Ok(vec![])`.
///
/// # Errors
/// Returns `Err(reason)` — a human-readable explanation, not a machine code
/// — if `bytes` fails to import into the fork, if the frame touched a
/// driver-only container, or if any new annotation fails one of the three
/// per-annotation checks above.
pub fn validate_annotation_update(
    doc: &LiveDoc,
    bytes: &[u8],
    sender_email: &str,
    keyring: &HashMap<String, PublicKey>,
) -> Result<Vec<Annotation>, String> {
    // The fork: see the module docs for why this is the whole point.
    let fork = doc.fork();
    fork.import_update(bytes)
        .map_err(|e| format!("frame did not import: {e}"))?;

    if fork.conversation_len() != doc.conversation_len()
        || fork.worktree_len() != doc.worktree_len()
    {
        return Err(
            "watchers may only write annotations, not conversation/worktree entries".to_string(),
        );
    }

    let existing_ids = doc.annotation_ids();
    let mut new_ids: Vec<String> = fork
        .annotation_ids()
        .difference(&existing_ids)
        .cloned()
        .collect();
    if new_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Deterministic order: not load-bearing for correctness, but keeps
    // error messages and the accepted-annotations ordering reproducible.
    new_ids.sort();

    let sender_key = keyring
        .get(sender_email)
        .ok_or_else(|| format!("no verifying key on file for '{sender_email}'"))?;

    // `LiveDoc::annotations` silently skips entries that fail to parse
    // (that is the right call for *reads*, per its own doc comment) — but
    // here a new entry that fails to parse is exactly the kind of bad
    // annotation this gate exists to catch, so it must reject, not vanish.
    // Look values up by id rather than trusting the parsed list's length.
    let parsed: HashMap<String, Annotation> = fork
        .annotations()
        .into_iter()
        .map(|a| (a.id.to_string(), a))
        .collect();

    let mut accepted = Vec::with_capacity(new_ids.len());
    for id in &new_ids {
        let annotation = parsed
            .get(id)
            .ok_or_else(|| format!("annotation {id} does not parse as an Annotation"))?;
        if annotation.author.email != sender_email {
            return Err(format!(
                "annotation {id} is authored by '{}' but the frame was sent by '{sender_email}'",
                annotation.author.email
            ));
        }
        if annotation.signature.is_none() {
            return Err(format!("annotation {id} is not signed"));
        }
        if !annotation.verifies_with(sender_key) {
            return Err(format!(
                "annotation {id} signature does not verify against '{sender_email}'s' key"
            ));
        }
        accepted.push(annotation.clone());
    }

    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use junto_kernel::{
        Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, Member, Span,
        Timestamp,
    };

    use super::*;
    use crate::Frame;

    fn keyring_of(
        email: &str,
        key: &junto_kernel::SigningKey,
    ) -> HashMap<String, junto_kernel::PublicKey> {
        HashMap::from([(email.to_string(), key.public_key())])
    }

    /// Same shape as `doc::tests::test_annotation`, but with a configurable
    /// author email — every test here cares about the interplay between the
    /// annotation's claimed author and the frame's authenticated sender.
    fn test_annotation_by(email: &str, body: &str) -> Annotation {
        Annotation {
            id: AnnotationId::new(),
            author: Member::human("Watcher", email),
            anchor: Anchor::Code(CodeAnchor {
                commit: CommitOid::new("a".repeat(40)).unwrap(),
                path: "src/anchor.rs".into(),
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

    #[test]
    fn valid_signed_annotation_is_accepted_and_returned() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let mut ann = test_annotation_by("w@x.com", "looks wrong");
        ann.sign(&key).unwrap();
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot(); // full snapshot is a valid update payload
        let got =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "looks wrong");
        // Server doc untouched until caller imports.
        assert!(server.annotations().is_empty());
    }

    #[test]
    fn unsigned_annotation_rejects_whole_frame() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let ann = test_annotation_by("w@x.com", "looks wrong"); // never signed
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot();
        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(err.contains("not signed"), "unexpected reason: {err}");
        assert!(server.annotations().is_empty());
    }

    #[test]
    fn wrong_author_email_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let mut ann = test_annotation_by("x@y.com", "looks wrong");
        ann.sign(&key).unwrap();
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot();
        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(err.contains("x@y.com"), "unexpected reason: {err}");
        assert!(server.annotations().is_empty());
    }

    #[test]
    fn signature_by_other_key_rejects() {
        let key1 = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let key2 = junto_kernel::SigningKey::from_secret_bytes([6; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let mut ann = test_annotation_by("w@x.com", "looks wrong");
        ann.sign(&key2).unwrap();
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot();
        // Keyring holds key1's public key for w@x.com, not key2's.
        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key1))
                .unwrap_err();
        assert!(err.contains("does not verify"), "unexpected reason: {err}");
        assert!(server.annotations().is_empty());
    }

    #[test]
    fn watcher_writing_conversation_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        watcher.push_conversation(serde_json::json!({"kind": "fake"}));
        let err = validate_annotation_update(
            &server,
            &watcher.export_snapshot(),
            "w@x.com",
            &keyring_of("w@x.com", &key),
        )
        .unwrap_err();
        assert!(err.contains("only write annotations"));
    }

    /// The fork guarantee, made explicit: a rejected frame must never
    /// leave any trace on the server's real document, on any of its three
    /// containers — not just `annotations` (already implied by the other
    /// tests above), but `conversation` and `worktree` too, since those are
    /// the containers whose growth this function checks against.
    #[test]
    fn rejected_frame_leaves_server_document_unchanged() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        server.push_conversation(serde_json::json!({"seq": 1}));
        server.push_worktree(serde_json::json!({"path": "src/lib.rs"}));
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let ann = test_annotation_by("w@x.com", "looks wrong"); // unsigned -> rejected
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot();

        let before_conversation = server.conversation_len();
        let before_worktree = server.worktree_len();

        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(!err.is_empty());

        assert_eq!(server.conversation_len(), before_conversation);
        assert_eq!(server.worktree_len(), before_worktree);
        assert!(server.annotations().is_empty());
        assert!(server.annotation_ids().is_empty());
    }

    #[test]
    fn frame_update_round_trips_base64() {
        let f = Frame::update(b"\x00\x01binary");
        let json = serde_json::to_string(&f).unwrap();
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.update_bytes().unwrap(), b"\x00\x01binary");
    }
}

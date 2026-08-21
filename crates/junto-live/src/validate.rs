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
//!
//! # What "only add annotations" actually has to mean
//!
//! A remote watcher is allowed to *add* annotations — not to touch anything
//! else. That sounds like it only constrains what is new, but loro's
//! `annotations` map is last-write-wins and its lists merge by op, not by
//! value: a synced watcher can overwrite an *existing* annotation id with
//! arbitrary content, delete it outright, or splice a driver-only list
//! (`conversation`/`worktree`) at an existing position — all without a
//! single new key appearing anywhere. A gate that only diffs added-id sets
//! and container lengths is blind to every one of those. So this function
//! checks two things, not one:
//! - every id already in `doc`'s `annotations` map must still map to
//!   byte-identical stored content in the fork ([`LiveDoc::annotation_raw`]);
//!   a revision is only ever accepted as a **new** id (see
//!   [`junto_kernel::Annotation::supersedes`]), never as an in-place
//!   rewrite of an old one;
//! - `conversation` and `worktree` must be byte-for-byte identical between
//!   `doc` and the fork ([`LiveDoc::conversation_matches`],
//!   [`LiveDoc::worktree_matches`]) — not merely the same length.

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
/// Checked, in order, on every call — including one that turns out to add
/// nothing at all:
/// 1. `sender_email` must have a key on file. Absence is not permission:
///    an unrecognized sender is rejected even if the frame's payload would
///    otherwise be accepted as a no-op.
/// 2. `conversation` and `worktree` must be unchanged, content-for-content,
///    between `doc` and the fork (see the module docs for why a length
///    check is not enough).
/// 3. Every annotation id `doc` already has must still hold byte-identical
///    stored content in the fork (see the module docs).
/// 4. Every id new to the fork must (a) parse as an [`Annotation`], (b)
///    carry an `id` equal to the map key it is stored under — otherwise a
///    sender could write a fresh key whose *own* id field aliases another
///    member's existing annotation, displacing it for any consumer that
///    keys by `id` rather than by map key — (c) have `author.email ==
///    sender_email`, (d) be signed, and (e) verify against
///    `keyring[sender_email]`.
///
/// Any single failure rejects the **whole frame** — there is no partial
/// acceptance. A frame that adds no annotations and touches nothing else is
/// legitimate (e.g. it may carry loro metadata with no visible new keys)
/// and returns `Ok(vec![])`.
///
/// # Errors
/// Returns `Err(reason)` — a human-readable explanation, not a machine code
/// — if `bytes` fails to import into the fork, if `sender_email` has no key
/// on file, if a driver-only container was touched, if an existing
/// annotation id's content changed, or if any new annotation fails one of
/// its five checks.
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

    // Checked first and unconditionally, before any other check can return
    // early: an unrecognized sender must never reach `Ok`, not even via a
    // frame that (superficially) changes nothing.
    let sender_key = keyring
        .get(sender_email)
        .ok_or_else(|| format!("no verifying key on file for '{sender_email}'"))?;

    if !fork.conversation_matches(doc) || !fork.worktree_matches(doc) {
        return Err(
            "watchers may only write annotations, not conversation/worktree entries".to_string(),
        );
    }

    // Every id the real doc already has must still resolve to the exact
    // same stored bytes in the fork — see the module docs for why an
    // added-ids-only diff cannot catch an overwrite or a deletion.
    let existing_ids = doc.annotation_ids();
    for id in &existing_ids {
        if fork.annotation_raw(id) != doc.annotation_raw(id) {
            return Err(format!(
                "annotation {id} was modified or removed by a watcher; only new annotation ids may be written"
            ));
        }
    }

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

    let mut accepted = Vec::with_capacity(new_ids.len());
    for id in &new_ids {
        // Only NEW ids are parsed here — existing ones were already
        // compared as raw bytes above, and re-parsing every historical
        // annotation on every incoming frame would cost O(session history)
        // work for O(1) real work.
        let annotation = fork
            .annotation_raw(id)
            .and_then(|raw| Annotation::from_canonical_bytes(raw.as_bytes()).ok())
            .ok_or_else(|| format!("annotation {id} does not parse as an Annotation"))?;
        if annotation.id.to_string() != *id {
            return Err(format!(
                "annotation stored under key {id} carries id {}; the map key must equal the annotation's own id",
                annotation.id
            ));
        }
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
                "annotation {id} signature does not verify against the key on file for '{sender_email}'"
            ));
        }
        accepted.push(annotation);
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
        assert_eq!(server.conversation_len(), 0);
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
    fn overwrite_existing_annotation_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let attacker_key = junto_kernel::SigningKey::from_secret_bytes([7; 32]);
        let server = LiveDoc::new();
        let mut original = test_annotation_by("w@x.com", "original");
        original.sign(&key).unwrap();
        server.insert_annotation(&original).unwrap();

        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        // Forge a replacement under the SAME id, signed by a different,
        // untrusted key. The added-ids diff alone never sees this: the id
        // is not new, only its content is.
        let mut forged = original.clone();
        forged.body = "tampered".into();
        forged.sign(&attacker_key).unwrap();
        watcher.insert_annotation(&forged).unwrap();
        let update = watcher.export_snapshot();

        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(
            err.contains("modified or removed"),
            "unexpected reason: {err}"
        );
        assert_eq!(server.annotations().len(), 1);
        assert_eq!(server.annotations()[0].body, "original");
    }

    #[test]
    fn wholesale_annotation_deletion_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let mut ann = test_annotation_by("w@x.com", "keep me");
        ann.sign(&key).unwrap();
        server.insert_annotation(&ann).unwrap();

        // A hand-rolled peer sharing the server's causal history (so it can
        // target the existing key) that deletes every annotation instead of
        // adding one. The added-ids diff is empty — a strict subset can
        // never be "new" — so this must be caught independently of it.
        let peer = loro::LoroDoc::new();
        peer.import(&server.export_snapshot()).unwrap();
        peer.get_map("annotations")
            .delete(&ann.id.to_string())
            .unwrap();
        peer.commit();
        let update = peer.export(loro::ExportMode::snapshot()).unwrap();

        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(
            err.contains("modified or removed"),
            "unexpected reason: {err}"
        );
        assert_eq!(server.annotations().len(), 1);
        assert_eq!(server.annotation_ids().len(), 1);
    }

    #[test]
    fn annotation_id_aliasing_an_existing_id_rejects() {
        let victim_key = junto_kernel::SigningKey::from_secret_bytes([9; 32]);
        let attacker_key = junto_kernel::SigningKey::from_secret_bytes([10; 32]);
        let server = LiveDoc::new();
        let mut victim = test_annotation_by("victim@x.com", "original");
        victim.sign(&victim_key).unwrap();
        server.insert_annotation(&victim).unwrap();

        // The attacker crafts an annotation authored by, and signed by,
        // themselves — so all three per-annotation checks would pass — but
        // whose internal `id` field aliases the victim's *existing* id,
        // written under a DIFFERENT, brand-new map key.
        // `LiveDoc::insert_annotation` can never produce this shape (it
        // always keys by the annotation's own id), so simulate it with a
        // raw peer sharing causal history.
        let mut forged = test_annotation_by("w@x.com", "attacker's note");
        forged.id = victim.id;
        forged.sign(&attacker_key).unwrap();
        let forged_json = String::from_utf8(forged.to_canonical_bytes().unwrap()).unwrap();

        let peer = loro::LoroDoc::new();
        peer.import(&server.export_snapshot()).unwrap();
        peer.get_map("annotations")
            .insert(
                "attacker-chosen-key-not-the-annotations-own-id",
                forged_json,
            )
            .unwrap();
        peer.commit();
        let update = peer.export(loro::ExportMode::snapshot()).unwrap();

        let err = validate_annotation_update(
            &server,
            &update,
            "w@x.com",
            &keyring_of("w@x.com", &attacker_key),
        )
        .unwrap_err();
        assert!(
            err.contains("must equal the annotation's own id"),
            "unexpected reason: {err}"
        );
        assert_eq!(server.annotations().len(), 1);
        assert_eq!(server.annotations()[0].body, "original");
    }

    #[test]
    fn conversation_splice_with_equal_length_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        server.push_conversation(serde_json::json!({"seq": 1}));

        // A hand-rolled peer that shares the server's causal history (so it
        // can target the existing element's position), then deletes it and
        // inserts a forged replacement of equal length in the same commit —
        // a length check alone cannot see this.
        let peer = loro::LoroDoc::new();
        peer.import(&server.export_snapshot()).unwrap();
        let list = peer.get_list("conversation");
        list.delete(0, 1).unwrap();
        list.insert(
            0,
            serde_json::json!({"seq": 999, "kind": "forged"}).to_string(),
        )
        .unwrap();
        peer.commit();
        let update = peer.export(loro::ExportMode::snapshot()).unwrap();

        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(
            err.contains("only write annotations"),
            "unexpected reason: {err}"
        );
        assert_eq!(server.conversation_len(), 1);
    }

    #[test]
    fn worktree_splice_with_equal_length_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        server.push_worktree(serde_json::json!({"path": "src/lib.rs"}));

        let peer = loro::LoroDoc::new();
        peer.import(&server.export_snapshot()).unwrap();
        let list = peer.get_list("worktree");
        list.delete(0, 1).unwrap();
        list.insert(0, serde_json::json!({"path": "forged.rs"}).to_string())
            .unwrap();
        peer.commit();
        let update = peer.export(loro::ExportMode::snapshot()).unwrap();

        let err =
            validate_annotation_update(&server, &update, "w@x.com", &keyring_of("w@x.com", &key))
                .unwrap_err();
        assert!(
            err.contains("only write annotations"),
            "unexpected reason: {err}"
        );
        assert_eq!(server.worktree_len(), 1);
    }

    #[test]
    fn keyring_absent_sender_rejects() {
        let key = junto_kernel::SigningKey::from_secret_bytes([5; 32]);
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        let mut ann = test_annotation_by("w@x.com", "looks wrong");
        ann.sign(&key).unwrap();
        watcher.insert_annotation(&ann).unwrap();
        let update = watcher.export_snapshot();

        // Empty keyring: sender has no key on file at all.
        let err =
            validate_annotation_update(&server, &update, "w@x.com", &HashMap::new()).unwrap_err();
        assert!(err.contains("no verifying key"), "unexpected reason: {err}");
        assert!(server.annotations().is_empty());
    }

    #[test]
    fn unknown_sender_with_no_changes_still_rejected() {
        let server = LiveDoc::new();
        let watcher = LiveDoc::new();
        watcher.import_update(&server.export_snapshot()).unwrap();
        // Watcher writes nothing at all — re-exports converged state.
        let update = watcher.export_snapshot();

        // A sender absent from the keyring must be rejected even though the
        // frame itself adds no new ids and touches no driver container —
        // authentication cannot be bypassed by sending a no-op frame.
        let err =
            validate_annotation_update(&server, &update, "ghost@nowhere.com", &HashMap::new())
                .unwrap_err();
        assert!(err.contains("no verifying key"), "unexpected reason: {err}");
    }
}

//! [`LiveDoc`] — one loro CRDT document per live agent session.
//!
//! junto's durable record is the append-only, signed [`junto_kernel::LedgerEntry`]
//! ledger (`docs/adr/0001`-`0011`): no CRDT, ever — hard constraint #3. That
//! record is settled history, written once and never mutated. A live agent
//! session is a different kind of thing: while it runs, its conversation and
//! worktree activity change many times a second and multiple watchers on
//! different machines want to follow along and comment *before* anything is
//! settled. A CRDT is the right tool for exactly that in-flight window — and
//! only that window: [`LiveDoc`] is thrown away (after being archived as one
//! final snapshot, a later slice) once the session ends. Nothing in this
//! crate touches `refs/junto/*` or the ledger; the live plane and the durable
//! record are deliberately kept apart.
//!
//! A [`LiveDoc`] holds three root containers:
//! - `conversation` (`LoroList`) — the session's live event stream.
//! - `worktree` (`LoroList`) — file-edit/diff events observed during the run.
//! - `annotations` (`LoroMap`) — the one container **any** authenticated
//!   member may write into, keyed by annotation id.
//!
//! Only `annotations` is genuinely multi-writer; `conversation` and
//! `worktree` are driver-writes-only in practice. That restriction is
//! deliberately **not** encoded in this data model — it is enforced as
//! policy by a later task, one layer up. Baking a single-writer assumption
//! into the CRDT itself would defeat the reason a CRDT was chosen here
//! rather than a plain log: a future rung of this project (letting a
//! second driver take over, or replaying a merged session) needs that door
//! left open.
//!
//! (Presence — who is watching right now — is not part of this document; it
//! rides on loro's separate `EphemeralStore` in a later task.)

use std::collections::HashSet;

use junto_kernel::Annotation;
use loro::{ExportMode, LoroDoc, LoroValue, Subscription};

/// Root container name for the session's live event stream.
const CONVERSATION: &str = "conversation";

/// Root container name for file-edit/diff events observed in the worktree
/// while the session runs.
const WORKTREE: &str = "worktree";

/// Root container name for the multi-author annotation map: key is an
/// [`junto_kernel::AnnotationId`]'s string form, value is that annotation's
/// own canonical-JSON bytes (see [`LiveDoc::insert_annotation`]).
const ANNOTATIONS: &str = "annotations";

/// One loro CRDT document for a single live agent session — see the module
/// docs for what it holds and why it exists apart from the ledger.
///
/// Every [`LiveDoc`] carries a fresh, random loro peer id, minted by
/// [`LoroDoc::new`]. That id is never derived from — or asserted to equal —
/// any member identity: two documents sharing a peer id corrupt each other's
/// history on merge, so identity here is deliberately a throwaway. The
/// authority for *who wrote an annotation* is the ed25519 signature carried
/// inside [`Annotation::signature`], verified independently of loro
/// (verification is a later task); the peer id is not, and must never
/// become, a proxy for authorship.
///
/// This struct is the only place in the crate — indeed, in the whole
/// workspace — that may name a `loro` type in a public signature, aside from
/// the [`Subscription`] handle callers must hold to keep a subscription
/// alive. Keeping `loro` otherwise absent from this type's public API is
/// what keeps a future CRDT-library swap confined to this one crate.
#[derive(Debug)]
pub struct LiveDoc {
    doc: LoroDoc,
}

impl Default for LiveDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveDoc {
    /// Start a new, empty live session document with a fresh random peer id
    /// (see the struct docs — never call `set_peer_id`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            doc: LoroDoc::new(),
        }
    }

    /// Append a conversation event and commit. `event` is stored as its own
    /// JSON text, not decomposed into loro fields — the watcher UI parses it
    /// back on read, so the wire shape lives with the event's own producer,
    /// not with this crate.
    pub fn push_conversation(&self, event: serde_json::Value) {
        self.push_event(CONVERSATION, event);
    }

    /// Append a worktree (file-edit/diff) event and commit. Same shape and
    /// policy as [`LiveDoc::push_conversation`], distinct container.
    pub fn push_worktree(&self, event: serde_json::Value) {
        self.push_event(WORKTREE, event);
    }

    /// Number of events pushed to `conversation` so far.
    #[must_use]
    pub fn conversation_len(&self) -> usize {
        self.doc.get_list(CONVERSATION).len()
    }

    /// Number of events pushed to `worktree` so far.
    #[must_use]
    pub fn worktree_len(&self) -> usize {
        self.doc.get_list(WORKTREE).len()
    }

    /// Insert (or overwrite) a signed annotation and commit. The map value is
    /// the annotation's own canonical-JSON bytes ([`Annotation::to_canonical_bytes`])
    /// stored **verbatim as a string**, never decomposed into loro fields:
    /// the signature covers exactly those bytes, so they must round-trip
    /// through the CRDT unmodified for [`Annotation::verifies_with`] to mean
    /// anything on the far side.
    ///
    /// # Errors
    /// Returns [`junto_kernel::Error::Serialization`] if `a` cannot be
    /// canonicalized (its own JCS encoding failed) or its canonical bytes are
    /// not valid UTF-8.
    pub fn insert_annotation(&self, a: &Annotation) -> junto_kernel::Result<()> {
        let bytes = a.to_canonical_bytes()?;
        let json = String::from_utf8(bytes)
            .map_err(|e| junto_kernel::Error::Serialization(e.to_string()))?;
        self.doc
            .get_map(ANNOTATIONS)
            .insert(&a.id.to_string(), json)
            .expect("LiveDoc never checks out a historical version, so editing cannot hit EditWhenDetached");
        self.doc.commit();
        Ok(())
    }

    /// Every annotation currently in the map, parsed back from its canonical
    /// bytes. Entries that fail to parse are **skipped, not errored** — a
    /// malformed value from a future annotation-schema version must not
    /// poison reads of the annotations that are still valid. Signature
    /// verification is not this method's job (a later task's concern).
    #[must_use]
    pub fn annotations(&self) -> Vec<Annotation> {
        let mut out = Vec::new();
        self.doc.get_map(ANNOTATIONS).for_each(|_key, value| {
            let Ok(LoroValue::String(json)) = value.into_value() else {
                return;
            };
            if let Ok(annotation) = Annotation::from_canonical_bytes(json.as_bytes()) {
                out.push(annotation);
            }
        });
        out
    }

    /// The keys of the annotation map — every id that has an entry,
    /// regardless of whether its value currently parses. Two documents with
    /// equal [`LiveDoc::annotation_ids`] have converged on the same set of
    /// annotation writes, independent of read-time parsing.
    #[must_use]
    pub fn annotation_ids(&self) -> HashSet<String> {
        self.doc
            .get_map(ANNOTATIONS)
            .keys()
            .map(|k| k.to_string())
            .collect()
    }

    /// Export the full current state as a self-contained snapshot — how a
    /// watcher joins a session already in progress (see the module docs) and
    /// how a driver would archive a finished one.
    #[must_use]
    pub fn export_snapshot(&self) -> Vec<u8> {
        self.doc
            .export(ExportMode::snapshot())
            .expect("exporting a snapshot of a live, non-shallow LoroDoc cannot fail")
    }

    /// Merge in a snapshot or update produced by [`LiveDoc::export_snapshot`]
    /// (or a future incremental export). Importing is commutative,
    /// associative, and idempotent — the same bytes may be imported more
    /// than once, and two documents that import each other's exports
    /// converge regardless of order (see the `concurrent_annotations_converge`
    /// test).
    ///
    /// # Errors
    /// Returns the loro import error, stringified — this crate's public API
    /// keeps `loro::LoroError` out of its own error types (see the struct
    /// docs on confining `loro` to this crate).
    pub fn import_update(&self, bytes: &[u8]) -> Result<(), String> {
        self.doc
            .import(bytes)
            .map(|_status| ())
            .map_err(|e| e.to_string())
    }

    /// Subscribe to this document's local updates (edits made through this
    /// handle, not imports). The returned [`Subscription`] must be held by
    /// the caller — dropping it cancels the subscription — which is why it
    /// is the one `loro` type this crate's public API cannot avoid naming.
    pub fn subscribe_local_update(
        &self,
        f: impl Fn(&Vec<u8>) -> bool + Send + Sync + 'static,
    ) -> Subscription {
        self.doc.subscribe_local_update(Box::new(f))
    }

    /// Fork this document: an independent copy sharing full history up to
    /// now, with its own fresh random peer id (loro's `fork` mints one, the
    /// same as [`LiveDoc::new`]) so the two never collide if both keep
    /// editing.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self {
            doc: self.doc.fork(),
        }
    }

    /// Shared body of [`LiveDoc::push_conversation`] and
    /// [`LiveDoc::push_worktree`]: push `event`'s JSON text onto the named
    /// list and commit.
    fn push_event(&self, container: &str, event: serde_json::Value) {
        // `serde_json::Value::to_string` cannot fail: `event` is already a
        // parsed value, not raw input being (re)validated.
        self.doc.get_list(container).push(event.to_string()).expect(
            "LiveDoc never checks out a historical version, so editing cannot hit EditWhenDetached",
        );
        self.doc.commit();
    }
}

#[cfg(test)]
mod tests {
    use junto_kernel::{
        Anchor, Annotation, AnnotationId, CodeAnchor, CommitOid, ContentDigest, Member, Span,
        Timestamp,
    };

    use super::*;

    fn test_annotation(body: &str) -> Annotation {
        Annotation {
            id: AnnotationId::new(),
            author: Member::human("Dan", "dan@example.com"),
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
    fn concurrent_annotations_converge() {
        let key_a = junto_kernel::SigningKey::from_secret_bytes([1; 32]);
        let key_b = junto_kernel::SigningKey::from_secret_bytes([2; 32]);
        let a = LiveDoc::new();
        let b = LiveDoc::new();
        // Seed b from a's snapshot (watchers join from a snapshot).
        b.import_update(&a.export_snapshot()).unwrap();
        let mut ann_a = test_annotation("from a");
        ann_a.sign(&key_a).unwrap();
        let mut ann_b = test_annotation("from b");
        ann_b.sign(&key_b).unwrap();
        a.insert_annotation(&ann_a).unwrap();
        b.insert_annotation(&ann_b).unwrap();
        // Cross-import full snapshots (idempotent, order-free).
        b.import_update(&a.export_snapshot()).unwrap();
        a.import_update(&b.export_snapshot()).unwrap();
        assert_eq!(a.annotation_ids(), b.annotation_ids());
        assert_eq!(a.annotations().len(), 2);
        // Value-level convergence on BOTH sides, not just a's: b must read
        // back exactly what a wrote, and vice versa.
        assert_eq!(b.annotations().len(), 2);
        // The signature is the whole point of storing annotations as opaque
        // canonical bytes rather than decomposed loro fields (see the
        // insert_annotation doc comment): it must survive the CRDT
        // round-trip byte-for-byte. Verify each annotation, read back from
        // the *other* document, against its original signer's key.
        let from_a_on_b = b
            .annotations()
            .into_iter()
            .find(|ann| ann.body == "from a")
            .expect("a's annotation present on b");
        assert!(from_a_on_b.verifies_with(&key_a.public_key()));
        let from_b_on_a = a
            .annotations()
            .into_iter()
            .find(|ann| ann.body == "from b")
            .expect("b's annotation present on a");
        assert!(from_b_on_a.verifies_with(&key_b.public_key()));
    }

    #[test]
    fn unparseable_annotation_entries_are_skipped_not_errored() {
        let key = junto_kernel::SigningKey::from_secret_bytes([3; 32]);
        let live = LiveDoc::new();
        let mut valid = test_annotation("valid");
        valid.sign(&key).unwrap();
        live.insert_annotation(&valid).unwrap();

        // A peer running some other (or future) annotation schema writes a
        // value into the same `annotations` map that does not parse as an
        // `Annotation` at all — the only realistic way this path fires,
        // since this crate's own writer (`insert_annotation`) never produces
        // one. Simulate that peer with a bare `loro::LoroDoc` sharing the
        // same container name and merge it in.
        let peer = LoroDoc::new();
        peer.get_map("annotations")
            .insert("not-an-annotation-id", "not json at all")
            .unwrap();
        peer.commit();
        live.import_update(&peer.export(ExportMode::snapshot()).unwrap())
            .unwrap();

        // The garbage key is present in the id set (the write itself
        // converged)...
        assert!(live.annotation_ids().contains("not-an-annotation-id"));
        assert_eq!(live.annotation_ids().len(), 2);
        // ...but `annotations()` skips it and still returns the co-resident
        // valid entry, rather than erroring or dropping everything.
        let parsed = live.annotations();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, valid.id);
    }

    #[test]
    fn conversation_events_survive_snapshot() {
        let a = LiveDoc::new();
        a.push_conversation(serde_json::json!({"kind": "assistant", "text": "hi", "seq": 1}));
        let b = LiveDoc::new();
        b.import_update(&a.export_snapshot()).unwrap();
        // Read back via the doc's deep value; one list entry.
        assert_eq!(b.conversation_len(), 1);
    }

    #[test]
    fn worktree_events_survive_snapshot() {
        let a = LiveDoc::new();
        a.push_worktree(serde_json::json!({"kind": "edit", "path": "src/lib.rs"}));
        let b = LiveDoc::new();
        b.import_update(&a.export_snapshot()).unwrap();
        assert_eq!(b.worktree_len(), 1);
        // The two lists are independent containers: worktree activity must
        // not show up as a conversation event or vice versa.
        assert_eq!(b.conversation_len(), 0);
    }

    #[test]
    fn fork_produces_an_independently_editable_replica_that_still_converges() {
        let key = junto_kernel::SigningKey::from_secret_bytes([4; 32]);
        let original = LiveDoc::new();
        original.push_conversation(serde_json::json!({"seq": 1}));

        let forked = original.fork();

        // Both replicas keep editing independently after the fork. If
        // `fork` shared the origin's peer id instead of minting its own
        // (the exact hazard the struct docs warn about), these concurrent
        // edits would corrupt one side's op history instead of merging
        // cleanly below.
        original.push_conversation(serde_json::json!({"seq": 2}));
        let mut ann = test_annotation("from fork");
        ann.sign(&key).unwrap();
        forked.insert_annotation(&ann).unwrap();

        original.import_update(&forked.export_snapshot()).unwrap();
        forked.import_update(&original.export_snapshot()).unwrap();

        assert_eq!(original.conversation_len(), 2);
        assert_eq!(forked.conversation_len(), 2);
        assert_eq!(original.annotation_ids(), forked.annotation_ids());
        assert_eq!(original.annotations().len(), 1);
    }

    #[test]
    fn subscribe_local_update_fires_on_local_commit() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let live = LiveDoc::new();
        let fire_count = std::sync::Arc::new(AtomicUsize::new(0));
        let saw_non_empty_payload = std::sync::Arc::new(AtomicBool::new(false));
        let fire_count_in_callback = fire_count.clone();
        let saw_non_empty_payload_in_callback = saw_non_empty_payload.clone();
        let _subscription = live.subscribe_local_update(move |bytes| {
            fire_count_in_callback.fetch_add(1, Ordering::SeqCst);
            if !bytes.is_empty() {
                saw_non_empty_payload_in_callback.store(true, Ordering::SeqCst);
            }
            true
        });

        live.push_conversation(serde_json::json!({"seq": 1}));

        assert_eq!(
            fire_count.load(Ordering::SeqCst),
            1,
            "callback should fire exactly once for one commit"
        );
        assert!(
            saw_non_empty_payload.load(Ordering::SeqCst),
            "the update payload delivered to the callback must be non-empty"
        );
    }
}

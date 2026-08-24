//! Anchors and annotations — the live-session-plane's vocabulary for pinning
//! a comment to a place.
//!
//! An [`Anchor`] locates the *thing being commented on*: either a span of a
//! file at a specific git commit ([`CodeAnchor`]) or a position within a live
//! collaborative stream ([`StreamAnchor`], e.g. a CRDT op id). An
//! [`Annotation`] is the comment itself — signed the same way a
//! [`crate::LedgerEntry`] is (`docs/adr/0033`), because a review comment
//! carries the same authorship-integrity requirement as any other kernel
//! record, even though it is not (yet) itself a ledger entry: the live
//! session plane is a faster, ephemeral layer in front of the ledger: it
//! re-anchors and settles into entries later (a subsequent slice), but must
//! be independently verifiable in the meantime.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ContentDigest, EntryId, Member, PublicKey, Signature, SigningKey, Timestamp};

/// A 1-indexed, inclusive line span within a file — editor line numbers, not
/// byte offsets, since that is what a human reviewer points at.
///
/// Serializes as a plain `{start, end}` object via the private [`SpanRepr`]
/// wire shape; **deserialization re-validates** through [`Span::new`] (via
/// [`TryFrom`]), so a malformed span cannot enter the kernel through the
/// canonical-bytes boundary (see [`crate::serial`]) — the same
/// re-validation-on-the-way-in rule as [`CommitOid`] and
/// [`crate::ContentDigest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "SpanRepr", into = "SpanRepr")]
pub struct Span {
    /// First line of the span, 1-indexed.
    pub start: u32,
    /// Last line of the span, 1-indexed and inclusive.
    pub end: u32,
}

impl Span {
    /// Construct a span, rejecting a zero `start` (lines are 1-indexed) or an
    /// `end` before `start` (an inverted range).
    ///
    /// # Errors
    /// Returns [`crate::Error::Invariant`] if `start == 0` or `end < start`.
    pub fn new(start: u32, end: u32) -> crate::Result<Self> {
        if start == 0 || end < start {
            return Err(crate::Error::Invariant(format!(
                "span must have start >= 1 and end >= start, got {start}..={end}"
            )));
        }
        Ok(Self { start, end })
    }
}

/// The bare `{start, end}` wire shape [`Span`] (de)serializes through, so
/// serde routes every deserialized value through [`Span::new`] for
/// re-validation instead of constructing a `Span` directly and skipping it.
#[derive(Serialize, Deserialize)]
struct SpanRepr {
    start: u32,
    end: u32,
}

impl From<Span> for SpanRepr {
    fn from(span: Span) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }
}

impl TryFrom<SpanRepr> for Span {
    type Error = crate::Error;

    fn try_from(repr: SpanRepr) -> crate::Result<Self> {
        Self::new(repr.start, repr.end)
    }
}

/// A git commit object id — 40 lowercase hex characters (SHA-1, matching
/// junto's git-refs substrate; not yet SHA-256-aware).
///
/// Serializes as a bare string; **deserialization re-validates** through
/// [`CommitOid::new`] (via [`TryFrom`]), so a malformed value cannot enter the
/// kernel through the canonical-bytes boundary (see [`crate::serial`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct CommitOid(String);

impl CommitOid {
    /// Construct a `CommitOid`, requiring exactly 40 lowercase hex characters.
    ///
    /// # Errors
    /// Returns [`crate::Error::Invariant`] if `value` is not 40 lowercase hex
    /// characters.
    pub fn new(value: impl Into<String>) -> crate::Result<Self> {
        let value = value.into();
        if value.len() != 40
            || !value
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(crate::Error::Invariant(
                "commit oid must be 40 lowercase hex chars".into(),
            ));
        }
        Ok(Self(value))
    }

    /// The underlying 40-hex-char string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The oid abbreviated to its first 7 characters — the length git itself
    /// prints — for surfaces that show a range rather than identify an
    /// object. Never fails: [`CommitOid::new`] is the only constructor and it
    /// admits exactly 40 characters.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..7]
    }
}

impl From<CommitOid> for String {
    fn from(oid: CommitOid) -> Self {
        oid.0
    }
}

impl TryFrom<String> for CommitOid {
    type Error = crate::Error;

    fn try_from(value: String) -> crate::Result<Self> {
        Self::new(value)
    }
}

/// An anchor into a file at a specific commit: the file's content
/// (`commit` + `path`, `blob`) and the line span within it. Pinning to a
/// commit and blob digest — rather than just a path and line range — is what
/// lets a later re-anchoring pass (a subsequent slice) detect that the
/// underlying line has drifted instead of silently pointing at the wrong code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodeAnchor {
    /// The commit the anchor was taken against.
    pub commit: CommitOid,
    /// Repo-relative path of the annotated file.
    pub path: String,
    /// Content digest of the file blob at `commit`, so drift is detectable
    /// the same way [`crate::ProvenanceRef::digest`] detects it for evidence.
    pub blob: ContentDigest,
    /// The annotated line span within the file.
    pub span: Span,
}

/// An anchor into a live collaborative stream (a loro-CRDT document, in the
/// forthcoming live-document slice): the session it belongs to and an
/// opaque, stream-defined operation id locating the position within it.
///
/// `session` is an [`EntryId`] rather than a dedicated session-id type: a
/// live session is identified by the `SessionStarted` entry that opened it
/// (`docs/adr` session model), so no separate `SessionId` newtype exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamAnchor {
    /// The `SessionStarted` entry identifying the live session.
    pub session: EntryId,
    /// Opaque, stream-defined operation id (e.g. a loro op id) locating the
    /// anchor within the session's live document. Not validated by the
    /// kernel: its shape is the live-document layer's concern.
    pub op_id: String,
}

/// An anchor into content that is **already in the record**: a line span inside
/// a stored artifact, identified by the entry that attached it and pinned by
/// that content's digest.
///
/// Unlike [`CodeAnchor`] this needs no commit and no re-anchoring pass. An
/// `ArtifactAttached` entry's own id *is* the artifact's id, its provenance
/// carries a [`ContentDigest`], and the ledger is append-only — so the content
/// under this anchor can never change and the anchor is **exact forever by
/// construction**. That is what distinguishes it from the `DocAnchor` still
/// owed for *mutable, external* documents, whose whole difficulty is detecting
/// that a quote has moved or been deleted.
///
/// # Choosing between this and [`CodeAnchor`] for a diff
///
/// A diff artifact can be pointed at either way, and which is correct depends
/// on whether the diff's **new side exists at a commit**:
///
/// - **Committed** — the new side is some commit's content, so
///   [`CodeAnchor`] is right: it names the file and lines *in the code*, which
///   survives the artifact and can be re-read from the repository.
/// - **Uncommitted** — the usual per-turn "uncommitted changes in …" artifact.
///   Its added lines exist at *no* commit, and the oid such a diff is captured
///   against is its **base**, so a `CodeAnchor` built from it would number
///   lines against a file that does not contain them. `RecordAnchor` is the
///   honest choice: it anchors the artifact's text, which is exactly what the
///   reviewer was looking at.
///
/// Preferring `CodeAnchor` whenever the commit is genuinely known keeps the
/// stronger claim available without ever fabricating one.
///
/// # Entries as well as artifacts
///
/// `entry` is any [`EntryId`], not only an `ArtifactAttached` one: a line of an
/// assertion's rationale is annotatable too (Dan's call, 2026-08-23). An
/// annotation on an entry is a **remark**, not a **verdict** — a verification
/// act (`Ratification`/`Park`/`Correction`) changes an entry's *standing*,
/// while an annotation says something about a line of its *text*, so the two
/// instruments overlap without conflicting.
///
/// The digest is well defined either way: an artifact's comes from its
/// provenance, and an entry's is the digest of its own canonical bytes
/// (`docs/adr/0008`), which an append-only log can never change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordAnchor {
    /// The entry whose content is annotated: an `ArtifactAttached` entry —
    /// whose id *is* the artifact's id — or any other entry, to annotate its
    /// own text.
    pub entry: EntryId,
    /// Digest of the annotated content, so an anchor and the bytes it was taken
    /// against can always be matched: an artifact's provenance digest, or the
    /// entry's own canonical-bytes digest.
    pub digest: ContentDigest,
    /// The annotated line span within the content.
    pub span: Span,
}

/// What an [`Annotation`] is pinned to: a span of code at a commit, a position
/// in a live stream, or a span of content already in the record. Tagged so the
/// shapes are distinguishable on the wire without a separate discriminant
/// field.
///
/// The tag is **internal** (`kind`), so adding a variant is additive: existing
/// `{"kind":"code",…}` and `{"kind":"stream",…}` values gain no field and move
/// no byte, leaving their canonical bytes — and therefore their signatures
/// (`docs/adr/0033`) — untouched.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// Pinned to a file span at a commit.
    Code(CodeAnchor),
    /// Pinned to a position in a live collaborative stream.
    Stream(StreamAnchor),
    /// Pinned to a line span of content already in the record.
    Record(RecordAnchor),
}

/// Identifies a single [`Annotation`] — the same transparent-UUID pattern as
/// [`crate::EntryId`] (`ids.rs`), since an annotation is a distinct kernel
/// noun from a ledger entry (it is not yet one; see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnnotationId(Uuid);

impl AnnotationId {
    /// Mint a fresh, random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AnnotationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AnnotationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for AnnotationId {
    type Err = crate::Error;

    /// Parse the `Display` form back into an id — how a surface turns a
    /// user- or wire-supplied annotation reference into a typed target.
    fn from_str(s: &str) -> crate::Result<Self> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|e| crate::Error::Invariant(format!("malformed annotation id '{s}': {e}")))
    }
}

/// A signed comment pinned to an [`Anchor`] — the live session plane's unit
/// of review feedback.
///
/// Mirrors [`crate::LedgerEntry`]'s sign/verify shape exactly
/// (`sign.rs` lines 158-210): a detached [`crate::Signature`] over this
/// annotation's own canonical bytes with the signature absent
/// ([`Annotation::signing_bytes`]). `author` carries a full [`Member`]
/// (not just an email) so an annotation carries the same authorship shape as
/// a ledger entry, ahead of the later slice that settles annotations into
/// entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    /// Stable, opaque identifier for this annotation.
    pub id: AnnotationId,
    /// Who wrote it — human or agent, same authorship shape as a
    /// [`crate::LedgerEntry`].
    pub author: Member,
    /// What it is pinned to.
    pub anchor: Anchor,
    /// The comment text.
    pub body: String,
    /// An optional quoted excerpt of the anchored content at annotation time,
    /// for display when the live anchor has since moved or the code has
    /// changed. Omitted from the canonical bytes when absent, matching the
    /// pre-existing omitted-field convention (see
    /// [`crate::ProvenanceRef::digest`]).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub excerpt: Option<String>,
    /// The annotation this one supersedes, if it is a correction or reply
    /// that replaces an earlier one. Omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub supersedes: Option<AnnotationId>,
    /// Whether this annotation demands attention before the anchored work
    /// can proceed (e.g. a blocking review comment), as opposed to an
    /// informational note.
    pub urgent: bool,
    /// When it was written.
    pub timestamp: Timestamp,
    /// A detached Ed25519 signature over this annotation's canonical bytes
    /// with this field absent (`docs/adr/0033`). Optional — unsigned
    /// annotations stay valid and simply never verify. Omitted from the
    /// canonical bytes when absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<Signature>,
}

impl Annotation {
    /// The signature preimage: this annotation's canonical bytes **with
    /// `signature` absent** — the same trick as
    /// [`crate::LedgerEntry::signing_bytes`].
    ///
    /// # Errors
    /// Returns [`crate::Error::Serialization`] if canonicalization fails.
    pub fn signing_bytes(&self) -> crate::Result<Vec<u8>> {
        if self.signature.is_none() {
            return self.to_canonical_bytes();
        }
        let mut unsigned = self.clone();
        unsigned.signature = None;
        unsigned.to_canonical_bytes()
    }

    /// Sign this annotation in place with `key`, replacing any existing
    /// signature.
    ///
    /// # Errors
    /// Returns [`crate::Error::Serialization`] if the preimage cannot be
    /// produced.
    pub fn sign(&mut self, key: &SigningKey) -> crate::Result<()> {
        self.signature = None;
        self.signature = Some(key.sign_bytes(&self.signing_bytes()?));
        Ok(())
    }

    /// Whether this annotation's signature verifies against `key`. Absent or
    /// malformed signatures are simply `false`, matching
    /// [`crate::LedgerEntry::verifies_with`]'s surfaced-fact-not-error stance.
    #[must_use]
    pub fn verifies_with(&self, key: &PublicKey) -> bool {
        let Some(signature) = &self.signature else {
            return false;
        };
        let Ok(bytes) = self.signing_bytes() else {
            return false;
        };
        key.verify_bytes(&bytes, signature)
    }

    /// Serialize to the canonical, deterministic byte form (JCS / RFC 8785,
    /// UTF-8 JSON) — same format as [`crate::LedgerEntry::to_canonical_bytes`]
    /// (see [`crate::serial`]).
    ///
    /// # Errors
    /// Returns [`crate::Error::Serialization`] if canonicalization fails.
    pub fn to_canonical_bytes(&self) -> crate::Result<Vec<u8>> {
        serde_json_canonicalizer::to_vec(self)
            .map_err(|e| crate::Error::Serialization(e.to_string()))
    }

    /// Parse an annotation from its canonical byte form, **re-validating**
    /// the embedded newtypes.
    ///
    /// # Errors
    /// Returns [`crate::Error::Serialization`] if the bytes are not valid
    /// canonical JSON or if an embedded value fails its invariant.
    pub fn from_canonical_bytes(bytes: &[u8]) -> crate::Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| crate::Error::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CodeAnchor` annotation with fixed, deterministic fields (except the
    /// random `id`, which the round-trip/tamper tests don't depend on).
    fn sample_annotation() -> Annotation {
        Annotation {
            id: AnnotationId::new(),
            author: Member::human("Dan", "dan@example.com"),
            anchor: Anchor::Code(CodeAnchor {
                commit: CommitOid::new("a".repeat(40)).unwrap(),
                path: "src/anchor.rs".into(),
                blob: ContentDigest::new("sha256:deadbeef").unwrap(),
                span: Span::new(3, 5).unwrap(),
            }),
            body: "consider a doc comment here".into(),
            excerpt: Some("pub struct Span".into()),
            supersedes: None,
            urgent: false,
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            signature: None,
        }
    }

    #[test]
    fn span_rejects_zero_and_inverted() {
        assert!(Span::new(0, 5).is_err());
        assert!(Span::new(7, 3).is_err());
        assert!(Span::new(3, 3).is_ok());
    }

    #[test]
    fn commit_oid_validates_shape() {
        assert!(CommitOid::new("a".repeat(40)).is_ok());
        assert!(CommitOid::new("A".repeat(40)).is_err()); // uppercase
        assert!(CommitOid::new("abc123").is_err()); // short
    }

    #[test]
    fn annotation_round_trips_and_signs() {
        let key = crate::SigningKey::from_secret_bytes([7; 32]);
        let mut a = sample_annotation(); // helper: builds a CodeAnchor annotation with fixed fields
        a.sign(&key).unwrap();
        let bytes = a.to_canonical_bytes().unwrap();
        let parsed = Annotation::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(a, parsed);
        assert!(parsed.verifies_with(&key.public_key()));
    }

    #[test]
    fn tampered_annotation_fails_verification() {
        let key = crate::SigningKey::from_secret_bytes([7; 32]);
        let mut a = sample_annotation();
        a.sign(&key).unwrap();
        a.body = "forged".into();
        assert!(!a.verifies_with(&key.public_key()));
    }

    #[test]
    fn unsigned_annotation_never_verifies() {
        let key = crate::SigningKey::from_secret_bytes([7; 32]);
        assert!(!sample_annotation().verifies_with(&key.public_key()));
    }

    #[test]
    fn stream_anchor_round_trips() {
        // Anchor::Stream serializes with kind tag and survives canonical round-trip.
        let a = Anchor::Stream(StreamAnchor {
            session: crate::EntryId::new(),
            op_id: "12@7".into(),
        });
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"kind\":\"stream\""));
        assert_eq!(a, serde_json::from_str::<Anchor>(&json).unwrap());
    }

    /// Builds the canonical bytes of an `Anchor::Code` annotation with the
    /// given (possibly invalid) span, constructed as raw JSON — bypassing
    /// `Span::new` entirely — to exercise the deserialize boundary itself.
    fn annotation_bytes_with_span(start: i64, end: i64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "id": AnnotationId::new().to_string(),
            "author": { "display_name": "Dan", "email": "dan@example.com", "kind": "Human" },
            "anchor": {
                "kind": "code",
                "commit": "a".repeat(40),
                "path": "src/anchor.rs",
                "blob": "sha256:deadbeef",
                "span": { "start": start, "end": end },
            },
            "body": "x",
            "urgent": false,
            "timestamp": 1_700_000_000_000i64,
        }))
        .unwrap()
    }

    #[test]
    fn malformed_span_is_rejected_on_deserialize() {
        // start == 0: rejected by Span::new's 1-indexed invariant.
        assert!(Annotation::from_canonical_bytes(&annotation_bytes_with_span(0, 0)).is_err());
        // end < start: rejected as an inverted range.
        assert!(Annotation::from_canonical_bytes(&annotation_bytes_with_span(7, 3)).is_err());
        // A valid span still deserializes.
        assert!(Annotation::from_canonical_bytes(&annotation_bytes_with_span(3, 3)).is_ok());
    }

    #[test]
    fn record_anchor_round_trips() {
        let a = Anchor::Record(RecordAnchor {
            entry: crate::EntryId::new(),
            digest: ContentDigest::new("sha256:deadbeef").unwrap(),
            span: Span::new(2, 4).unwrap(),
        });
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"kind\":\"record\""), "{json}");
        assert_eq!(a, serde_json::from_str::<Anchor>(&json).unwrap());
    }

    #[test]
    fn adding_the_record_variant_moved_no_existing_anchor_bytes() {
        // THE PERMANENCE CLAIM the `Anchor::Record` proposal rests on, asserted
        // rather than argued: `Anchor` is internally tagged, so an existing
        // value gains no field and moves no byte when a third variant exists
        // beside it. If this ever fails, every annotation signed before the
        // variant landed stops verifying (`docs/adr/0033`).
        //
        // The literal below is the wire form of an annotation actually archived
        // by a live session on 2026-08-23, copied verbatim from its live-plane
        // snapshot rather than reconstructed.
        let archived = r#"{"blob":"sha256:unpinned","commit":"eccb1dad8bce59205d2e57896680d00433d4edb8","kind":"code","path":"lib.rs","span":{"end":4,"start":2}}"#;
        let anchor: Anchor = serde_json::from_str(archived).expect("still deserializes");
        match &anchor {
            Anchor::Code(code) => {
                assert_eq!(code.path, "lib.rs");
                assert_eq!((code.span.start, code.span.end), (2, 4));
            }
            other => panic!("expected a code anchor, got {other:?}"),
        }
        // Re-serializing yields the same field set — no new key, no `record`
        // discriminant leaking into a code anchor.
        let again: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&anchor).unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(archived).unwrap();
        assert_eq!(again, expected, "an existing anchor's bytes must not move");
    }

    #[test]
    fn a_record_anchor_needs_no_commit_and_a_code_anchor_still_does() {
        // The distinction Dan drew: a committed diff should be pinned to the
        // code at its commit, while an UNCOMMITTED diff's added lines exist at
        // no commit at all and must be anchored as record content instead. The
        // types enforce it — `RecordAnchor` has no commit field to fabricate.
        let record = RecordAnchor {
            entry: crate::EntryId::new(),
            digest: ContentDigest::new("sha256:abc").unwrap(),
            span: Span::new(1, 1).unwrap(),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&Anchor::Record(record)).unwrap()).unwrap();
        assert!(
            json.get("commit").is_none(),
            "a record anchor must never carry a commit: {json}"
        );
        assert_eq!(json["kind"], "record");
    }
}

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
/// Serializes as a plain `{start, end}` struct (no newtype string form is
/// needed: both fields are already primitives); validated on construction via
/// [`Span::new`] rather than on deserialize, since a `Span` only ever appears
/// nested inside a [`CodeAnchor`] whose containing [`Annotation`] revalidates
/// nothing else structurally either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// What an [`Annotation`] is pinned to: a span of code at a commit, or a
/// position in a live stream. Tagged so the two shapes are distinguishable
/// on the wire without a separate discriminant field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// Pinned to a file span at a commit.
    Code(CodeAnchor),
    /// Pinned to a position in a live collaborative stream.
    Stream(StreamAnchor),
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
}

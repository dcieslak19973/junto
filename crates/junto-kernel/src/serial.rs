//! The canonical byte form of a [`LedgerEntry`] — junto's durable record format.
//!
//! Entries are stored under git refs (`refs/junto/*`, hard constraint #3) and
//! will eventually be content-addressed, so their byte form must be
//! **deterministic and cross-platform-stable**: the same entry written on
//! Windows and macOS has to produce identical bytes, or dedup/ordering/hashing
//! silently break.
//!
//! The format is **Canonical JSON per JCS / RFC 8785** (see `docs/adr/0008`):
//! plain JSON (readable, `git show`-diffable — the human+agent-navigable record
//! value) with the canonicalization rules that make it deterministic *by spec*
//! rather than by struct field order — keys sorted by UTF-16 code-unit order,
//! all insignificant whitespace removed. Two consequences worth knowing:
//! compact JCS contains **no raw newline bytes**, sidestepping the CRLF hazard
//! CLAUDE.md flags; and the validated newtypes ([`crate::ProvenanceRef`]'s
//! `Uri`/`ContentDigest`) **re-validate on the way in**, so a malformed value
//! cannot enter the kernel through this boundary.
//!
//! This module owns the format decision in one place: serialize via JCS, parse
//! via `serde_json` (any RFC-8785 output is valid JSON). Callers — notably the
//! future git-refs substrate — use [`LedgerEntry::to_canonical_bytes`] /
//! [`LedgerEntry::from_canonical_bytes`] rather than touching a serializer.

use crate::{Error, LedgerEntry, Result};

impl LedgerEntry {
    /// Serialize to the canonical, deterministic byte form (JCS / RFC 8785,
    /// UTF-8 JSON) used as the durable git-refs record.
    ///
    /// # Errors
    /// Returns [`Error::Serialization`] if canonicalization fails.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_json_canonicalizer::to_vec(self).map_err(|e| Error::Serialization(e.to_string()))
    }

    /// Parse an entry from its canonical byte form, **re-validating** the
    /// embedded newtypes (e.g. a non-empty `Uri`, a well-formed `ContentDigest`).
    ///
    /// # Errors
    /// Returns [`Error::Serialization`] if the bytes are not valid canonical
    /// JSON or if an embedded value fails its invariant.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| Error::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ApprovalRequirement, ChannelId, ContentDigest, EntryId, EntryPayload, LedgerEntry, Member,
        ProvenanceRef, Timestamp, Uri,
    };

    /// Build an entry with the given payload, authored by a fixed human at a
    /// fixed time — deterministic except for the (random) ids, which the
    /// round-trip tests don't depend on.
    fn entry(payload: EntryPayload) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Ada Lovelace", "ada@example.com"),
            timestamp: Timestamp::from_millis(1_717_900_000_000),
            payload,
        }
    }

    fn provenance_with_digest() -> ProvenanceRef {
        ProvenanceRef::with_digest(
            Uri::new("git:abc123").expect("valid uri"),
            ContentDigest::new("sha256:deadbeef").expect("valid digest"),
        )
    }

    fn assert_round_trips(entry: &LedgerEntry) {
        let bytes = entry.to_canonical_bytes().expect("serialize");
        let parsed = LedgerEntry::from_canonical_bytes(&bytes).expect("deserialize");
        assert_eq!(entry, &parsed);
    }

    #[test]
    fn round_trips_every_payload_kind() {
        let target = EntryId::new();

        assert_round_trips(&entry(EntryPayload::ChannelOpened {
            name: "junto-dev".into(),
        }));
        assert_round_trips(&entry(EntryPayload::MemberAdded {
            member: Member::agent("Claude Code", "claude-code@anthropic.com"),
        }));
        // Assertion with a digest-bearing provenance ref.
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "the sky is blue".into(),
            rationale: "observed at noon".into(),
            provenance: vec![provenance_with_digest()],
        }));
        // Assertion with a digest-less provenance ref (exercises the omitted field).
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "water is wet".into(),
            rationale: "by definition".into(),
            provenance: vec![ProvenanceRef::new(
                Uri::new("file://notes.md").expect("uri"),
            )],
        }));
        assert_round_trips(&entry(EntryPayload::Ratification {
            target,
            rationale: "confirmed".into(),
        }));
        assert_round_trips(&entry(EntryPayload::Park {
            target,
            rationale: "dead end".into(),
        }));
        assert_round_trips(&entry(EntryPayload::Correction {
            target,
            statement: "the sky is azure".into(),
            rationale: "more precise".into(),
        }));
        // Proposal exercising each ApprovalRequirement shape.
        for requirement in [
            ApprovalRequirement::Auto,
            ApprovalRequirement::Count(2),
            ApprovalRequirement::AllOf(vec![
                Member::human("Alice", "alice@example.com"),
                Member::agent("Bot", "bot@example.com"),
            ]),
        ] {
            assert_round_trips(&entry(EntryPayload::Proposal {
                action: "merge PR #1".into(),
                rationale: "ready".into(),
                provenance: vec![provenance_with_digest()],
                requirement,
            }));
        }
        assert_round_trips(&entry(EntryPayload::Approval {
            target,
            rationale: "looks good".into(),
        }));
        assert_round_trips(&entry(EntryPayload::Rejection {
            target,
            rationale: "needs work".into(),
        }));
    }

    #[test]
    fn serialization_is_deterministic() {
        let e = entry(EntryPayload::Assertion {
            statement: "stable".into(),
            rationale: "twice".into(),
            provenance: vec![provenance_with_digest()],
        });
        assert_eq!(
            e.to_canonical_bytes().expect("first"),
            e.to_canonical_bytes().expect("second"),
        );
    }

    #[test]
    fn keys_are_jcs_sorted() {
        let e = entry(EntryPayload::Assertion {
            statement: "x".into(),
            rationale: "y".into(),
            provenance: vec![],
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        // JCS sorts object keys; the envelope keys must appear alphabetically,
        // proving the canonical scheme (not struct declaration order) is in effect.
        let author = json.find("\"author\"").expect("author key");
        let channel = json.find("\"channel\"").expect("channel key");
        let id = json.find("\"id\"").expect("id key");
        let payload = json.find("\"payload\"").expect("payload key");
        let timestamp = json.find("\"timestamp\"").expect("timestamp key");
        assert!(author < channel && channel < id && id < payload && payload < timestamp);
    }

    #[test]
    fn canonical_form_has_no_raw_newline_bytes() {
        // A rationale containing CRLF must be JSON-escaped to the bytes \r \n,
        // never emitted as raw 0x0D / 0x0A — the cross-platform CRLF guard.
        let e = entry(EntryPayload::Assertion {
            statement: "multi".into(),
            rationale: "line one\r\nline two".into(),
            provenance: vec![],
        });
        let bytes = e.to_canonical_bytes().expect("serialize");
        assert!(
            !bytes.contains(&b'\r'),
            "canonical bytes must not contain CR"
        );
        assert!(
            !bytes.contains(&b'\n'),
            "canonical bytes must not contain LF"
        );
        // The escaped sequence is present instead.
        let json = String::from_utf8(bytes).expect("utf8");
        assert!(json.contains("\\r\\n"));
    }

    #[test]
    fn empty_uri_is_rejected_on_deserialize() {
        // Hand-craft JSON with an empty provenance uri; the newtype invariant
        // (non-empty) must be re-checked, not bypassed.
        let json = r#"{
            "author": {"display_name": "Ada", "email": "ada@example.com", "kind": "Human"},
            "channel": "00000000-0000-0000-0000-000000000000",
            "id": "00000000-0000-0000-0000-000000000001",
            "payload": {"Assertion": {"statement": "s", "rationale": "r",
                "provenance": [{"uri": ""}]}},
            "timestamp": 0
        }"#;
        assert!(LedgerEntry::from_canonical_bytes(json.as_bytes()).is_err());
    }

    #[test]
    fn malformed_digest_is_rejected_on_deserialize() {
        // A digest without the `algorithm:` prefix must fail the invariant.
        let json = r#"{
            "author": {"display_name": "Ada", "email": "ada@example.com", "kind": "Human"},
            "channel": "00000000-0000-0000-0000-000000000000",
            "id": "00000000-0000-0000-0000-000000000001",
            "payload": {"Assertion": {"statement": "s", "rationale": "r",
                "provenance": [{"uri": "git:abc", "digest": "deadbeef"}]}},
            "timestamp": 0
        }"#;
        assert!(LedgerEntry::from_canonical_bytes(json.as_bytes()).is_err());
    }

    #[test]
    fn golden_canonical_form_is_byte_stable() {
        // A fixed canonical JSON string (deterministic ids/timestamp) must
        // deserialize and re-serialize to byte-identical output — pinning field
        // order and format stability without needing a from-UUID constructor.
        let golden = concat!(
            "{",
            r#""author":{"display_name":"Ada Lovelace","email":"ada@example.com","kind":"Human"},"#,
            r#""channel":"00000000-0000-0000-0000-000000000000","#,
            r#""id":"00000000-0000-0000-0000-000000000001","#,
            r#""payload":{"Assertion":{"provenance":[{"uri":"git:abc"}],"rationale":"r","statement":"s"}},"#,
            r#""timestamp":42"#,
            "}",
        );
        let parsed = LedgerEntry::from_canonical_bytes(golden.as_bytes()).expect("parse golden");
        let reserialized =
            String::from_utf8(parsed.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert_eq!(golden, reserialized);
    }
}

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
            signature: None,
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
            name: Some("junto-dev".into()),
        }));
        assert_round_trips(&entry(EntryPayload::MemberAdded {
            member: Member::agent("Claude Code", "claude-code@anthropic.com"),
        }));
        assert_round_trips(&entry(EntryPayload::ChannelClosed {
            rationale: "inquiry finished".into(),
        }));
        assert_round_trips(&entry(EntryPayload::ChannelReopened {
            rationale: "it resumed".into(),
        }));
        // Lineage edges (docs/adr/0027) — four kinds, two per edge.
        assert_round_trips(&entry(EntryPayload::DivergedFrom {
            parent: ChannelId::new(),
            at: Some(target),
        }));
        // DivergedFrom with no anchor point (exercises the omitted field).
        assert_round_trips(&entry(EntryPayload::DivergedFrom {
            parent: ChannelId::new(),
            at: None,
        }));
        assert_round_trips(&entry(EntryPayload::ChildDiverged {
            child: ChannelId::new(),
        }));
        assert_round_trips(&entry(EntryPayload::ConvergedInto {
            target: ChannelId::new(),
        }));
        assert_round_trips(&entry(EntryPayload::ConvergenceReceived {
            source: ChannelId::new(),
        }));
        // Assertion with a digest-bearing provenance ref.
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "the sky is blue".into(),
            rationale: "observed at noon".into(),
            provenance: vec![provenance_with_digest()],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        }));
        // Assertion with a digest-less provenance ref (exercises the omitted field).
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "water is wet".into(),
            rationale: "by definition".into(),
            provenance: vec![ProvenanceRef::new(
                Uri::new("file://notes.md").expect("uri"),
            )],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        }));
        // Assertion carrying a decision frame (docs/adr/0019) — the frame
        // round-trips, unchosen options included.
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "the fix holds".into(),
            rationale: "tests pass".into(),
            provenance: vec![],
            frame: Some(crate::DecisionFrame {
                options: vec![
                    crate::FrameOption {
                        label: "verified".into(),
                        act: crate::FrameAct::Ratify,
                        rationale: "CI green and reviewed".into(),
                    },
                    crate::FrameOption {
                        label: "not convinced".into(),
                        act: crate::FrameAct::Park,
                        rationale: "evidence insufficient".into(),
                    },
                ],
            }),
            session: None,
            kind: None,
            answers: None,
        }));
        // The new assertion facts: a finding that names its session and the
        // open entry it answers, and the all-absent case that must keep
        // pre-change bytes.
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "the ranker is reusable".into(),
            rationale: "IDF overlap, no deps".into(),
            provenance: vec![],
            frame: None,
            session: Some(target),
            kind: Some(crate::AssertionKind::Finding),
            answers: Some(vec![target]),
        }));
        assert_round_trips(&entry(EntryPayload::Assertion {
            statement: "no new facts".into(),
            rationale: "legacy shape".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: None,
            answers: None,
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
                frame: None,
                kind: None,
                requirement,
            }));
        }
        assert_round_trips(&entry(EntryPayload::GateExecuted {
            target,
            success: true,
            note: "https://github.com/x/y/pull/1".into(),
        }));
        // A proposal carrying a playbook `kind` tag (docs/adr/0029).
        assert_round_trips(&entry(EntryPayload::Proposal {
            action: "Open a pull request".into(),
            rationale: "verified green".into(),
            provenance: vec![],
            frame: None,
            kind: Some("code-pr.open-pr".into()),
            requirement: ApprovalRequirement::Count(1),
        }));
        assert_round_trips(&entry(EntryPayload::Approval {
            target,
            rationale: "looks good".into(),
        }));
        assert_round_trips(&entry(EntryPayload::Rejection {
            target,
            rationale: "needs work".into(),
        }));
        // The Agent Session family: start, every state an update can carry,
        // and an artifact with digest-bearing provenance.
        assert_round_trips(&entry(EntryPayload::SessionStarted {
            intent: "fix the flaky test".into(),
        }));
        for state in [
            crate::SessionState::Working,
            crate::SessionState::Blocked,
            crate::SessionState::AwaitingApproval,
            crate::SessionState::Done,
            crate::SessionState::Error,
        ] {
            assert_round_trips(&entry(EntryPayload::SessionUpdated {
                target,
                state,
                note: "progress".into(),
            }));
        }
        // A recorded commit range, with and without the optional branch: the
        // absent case must round-trip through the `skip_serializing_if` path
        // that keeps it out of the canonical bytes entirely.
        for branch in [Some("junto/9f2".to_string()), None] {
            assert_round_trips(&entry(EntryPayload::SessionCommitted {
                target,
                branch,
                base: crate::CommitOid::new("0f1e2d3c4b5a69788796a5b4c3d2e1f009182736")
                    .expect("valid oid"),
                head: crate::CommitOid::new("1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d")
                    .expect("valid oid"),
            }));
        }
        assert_round_trips(&entry(EntryPayload::ArtifactAttached {
            target,
            kind: "diff".into(),
            description: "the fix as a unified diff".into(),
            provenance: vec![provenance_with_digest()],
        }));
        assert_round_trips(&entry(EntryPayload::SubjectAttached {
            subject: crate::Subject::new(
                crate::SubjectKind::Repo,
                Uri::new("git+https://github.com/dcieslak19973/junto.git").expect("valid uri"),
            ),
        }));
        // A Subject with a pinned digest (exercises the Some(digest) branch).
        assert_round_trips(&entry(EntryPayload::SubjectAttached {
            subject: crate::Subject::with_digest(
                crate::SubjectKind::Document,
                Uri::new("file:///notes/spec.md").expect("valid uri"),
                ContentDigest::new("sha256:deadbeef").expect("valid digest"),
            ),
        }));
        assert_round_trips(&entry(EntryPayload::SubjectDetached { target }));
    }

    #[test]
    fn an_unnamed_channel_omits_the_name_from_its_canonical_bytes() {
        let unnamed = entry(EntryPayload::ChannelOpened { name: None });
        assert_round_trips(&unnamed);
        let text =
            String::from_utf8(unnamed.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(
            !text.contains("\"name\":"),
            "an absent name must not appear in the canonical bytes \
             (checked as the JSON key, since the author's `display_name` \
             field also contains the substring \"name\"): {text}"
        );
    }

    #[test]
    fn a_named_channels_canonical_bytes_are_unchanged_by_the_name_becoming_optional() {
        let named = entry(EntryPayload::ChannelOpened {
            name: Some("junto-dev".into()),
        });
        let text = String::from_utf8(named.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(
            text.contains(r#""name":"junto-dev""#),
            "a present name must serialize exactly as before: {text}"
        );
    }

    #[test]
    fn an_absent_subject_digest_is_omitted_from_the_canonical_bytes() {
        let without = entry(EntryPayload::SubjectAttached {
            subject: crate::Subject::new(
                crate::SubjectKind::Document,
                Uri::new("file:///notes/spec.md").expect("valid uri"),
            ),
        });
        let bytes = without.to_canonical_bytes().expect("serialize");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(
            !text.contains("digest"),
            "an absent digest must not appear in the canonical bytes: {text}"
        );
    }

    #[test]
    fn absent_frame_leaves_canonical_bytes_unchanged() {
        // docs/adr/0019: the frame field is omitted entirely when absent, so
        // every pre-frame entry's canonical bytes — and thus dedup, ordering,
        // and any future content addressing — are untouched.
        let e = entry(EntryPayload::Assertion {
            statement: "no frame".into(),
            rationale: "plain".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(!json.contains("\"frame\""), "{json}");
    }

    #[test]
    fn absent_assertion_facts_leave_canonical_bytes_unchanged() {
        // Same additive rule as `frame` and `ChannelOpened::name`: an entry
        // that carries none of the new facts must serialise exactly as it did
        // before they existed, or every pre-change signature breaks.
        let e = entry(EntryPayload::Assertion {
            statement: "plain".into(),
            rationale: "plain".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: None,
            answers: None,
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        // `author.kind` ("Human"/"Agent") always appears in the envelope, so
        // the bare-substring check for the new assertion `kind` field must
        // scope to the payload — same collision the `name` test above notes
        // for `author.display_name`.
        let payload = &json[json.find("\"payload\"").expect("payload key")..];
        assert!(!payload.contains("\"session\""), "{json}");
        assert!(!payload.contains("\"kind\""), "{json}");
        assert!(!payload.contains("\"answers\""), "{json}");
    }

    #[test]
    fn assertion_kind_is_snake_case_on_the_wire() {
        // The record is read by humans and by other tools; "finding" is the
        // wire form, not "Finding".
        let e = entry(EntryPayload::Assertion {
            statement: "x".into(),
            rationale: "y".into(),
            provenance: vec![],
            frame: None,
            session: None,
            kind: Some(crate::AssertionKind::Finding),
            answers: None,
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(json.contains("\"kind\":\"finding\""), "{json}");
    }

    #[test]
    fn an_absent_commit_range_branch_is_omitted_from_the_canonical_bytes() {
        // Same additive rule as `signature`, `DivergedFrom::at`,
        // `Assertion::frame` and `ChannelOpened::name`: the branch is a
        // convenience label, so when git could not name one the key is absent
        // rather than null, and the range's bytes stay minimal.
        let e = entry(EntryPayload::SessionCommitted {
            target: EntryId::new(),
            branch: None,
            base: crate::CommitOid::new("0f1e2d3c4b5a69788796a5b4c3d2e1f009182736")
                .expect("valid oid"),
            head: crate::CommitOid::new("1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d")
                .expect("valid oid"),
        });
        let json = String::from_utf8(e.to_canonical_bytes().expect("serialize")).expect("utf8");
        assert!(!json.contains("\"branch\""), "{json}");
        // The oids themselves must be present and unabbreviated: `short()` is
        // for surfaces, never for the record.
        assert!(
            json.contains("0f1e2d3c4b5a69788796a5b4c3d2e1f009182736"),
            "{json}"
        );
    }

    #[test]
    fn serialization_is_deterministic() {
        let e = entry(EntryPayload::Assertion {
            statement: "stable".into(),
            rationale: "twice".into(),
            provenance: vec![provenance_with_digest()],
            frame: None,
            session: None,
            kind: None,
            answers: None,
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
            frame: None,
            session: None,
            kind: None,
            answers: None,
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
            frame: None,
            session: None,
            kind: None,
            answers: None,
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

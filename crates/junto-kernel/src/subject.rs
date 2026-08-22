//! **Subjects** — what a Channel is *about* (`docs/adr/0014`, and the spec at
//! `docs/superpowers/specs/2026-08-21-multiplayer-first-rethink-design.md` §1).
//!
//! A Subject is durable and **portable**: it names a thing by URI, never by a
//! path on somebody's disk. How *this* machine resolves a Subject to something
//! it can read or run in is a **Mount**, which is machine-local config and
//! never enters the ledger (`domain-model.md:32`).
//!
//! A channel may have zero, one, or many Subjects. A git repo is simply the
//! kind that supports every capability; a document supports fewer, and says so.

use serde::{Deserialize, Serialize};

use crate::provenance::{ContentDigest, Uri};

/// The kinds of thing a Channel can be about.
///
/// Deliberately a closed enum in the kernel: adding a kind is a kernel change,
/// because each kind's capabilities are kernel-visible. The *providers* that
/// reach these things (a forge, a chat connector, a knowledge connector) stay
/// behind adapters — no vendor name appears here (constraint #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubjectKind {
    /// A git repository. The only kind that can be executed in and diffed.
    Repo,
    /// A document: a file, a wiki page, a spec. Readable and anchorable,
    /// never executable.
    Document,
}

/// One thing a Channel is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    /// What kind of thing this is.
    pub kind: SubjectKind,
    /// Where it lives, machine-independently.
    pub uri: Uri,
    /// Its content digest as of attachment, so later drift is detectable.
    /// Absent for subjects with no stable content hash (a live repo).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<ContentDigest>,
}

impl Subject {
    /// A subject with no pinned version.
    #[must_use]
    pub fn new(kind: SubjectKind, uri: Uri) -> Self {
        Self {
            kind,
            uri,
            digest: None,
        }
    }

    /// A subject pinned to the content it had when it was attached.
    #[must_use]
    pub fn with_digest(kind: SubjectKind, uri: Uri, digest: ContentDigest) -> Self {
        Self {
            kind,
            uri,
            digest: Some(digest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentDigest, Uri};

    #[test]
    fn a_subject_carries_kind_uri_and_optional_digest() {
        let uri = Uri::new("git+https://github.com/dcieslak19973/junto.git").expect("valid uri");
        let subject = Subject::new(SubjectKind::Repo, uri.clone());
        assert_eq!(subject.kind, SubjectKind::Repo);
        assert_eq!(subject.uri, uri);
        assert!(subject.digest.is_none());
    }

    #[test]
    fn a_document_subject_pins_the_version_it_was_attached_at() {
        let uri = Uri::new("file:///notes/spec.md").expect("valid uri");
        let digest = ContentDigest::new("sha256:deadbeef").expect("valid digest");
        let subject = Subject::with_digest(SubjectKind::Document, uri, digest.clone());
        assert_eq!(subject.kind, SubjectKind::Document);
        assert_eq!(subject.digest, Some(digest));
    }

    #[test]
    fn subject_kinds_round_trip_through_json() {
        for kind in [SubjectKind::Repo, SubjectKind::Document] {
            let text = serde_json::to_string(&kind).expect("serialize");
            let parsed: SubjectKind = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(kind, parsed);
        }
    }
}

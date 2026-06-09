//! Provenance — where an assertion's evidence comes from.
//!
//! [`ProvenanceRef`] is a **relation**, not a standalone entity: it binds an
//! [`crate::EntryPayload::Assertion`] to the evidence that backs it. This slice
//! keeps it minimal (`docs/adr/0005`): a `uri` locating the evidence plus
//! an optional self-describing `digest` so drift can be detected if the URI's
//! content later changes.
//!
//! Deliberately deferred: a typed evidence enum (Artifact | GitObject | Session
//! | External), digest *computation* (we store, we do not yet hash), and fully
//! re-runnable provenance. Alternatives stay in an assertion's `rationale`
//! until a second Playbook proves the richer shape is needed.

use serde::{Deserialize, Serialize};

/// A location for a piece of evidence — an artifact path, a git object, a
/// session record, an external URL. Validated only as non-empty for now.
///
/// Serializes as a bare string; **deserialization re-validates** through
/// [`Uri::new`] (via [`TryFrom`]), so a malformed value cannot enter the kernel
/// through the canonical-bytes boundary (see [`crate::serial`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Uri(String);

impl Uri {
    /// Construct a `Uri`, rejecting the empty string.
    ///
    /// # Errors
    /// Returns [`crate::Error::Invariant`] if `value` is empty.
    pub fn new(value: impl Into<String>) -> crate::Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(crate::Error::Invariant(
                "provenance uri must be non-empty".into(),
            ));
        }
        Ok(Self(value))
    }

    /// The underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Uri> for String {
    fn from(uri: Uri) -> Self {
        uri.0
    }
}

impl TryFrom<String> for Uri {
    type Error = crate::Error;

    fn try_from(value: String) -> crate::Result<Self> {
        Self::new(value)
    }
}

/// A self-describing content digest, stored as `algorithm:value`
/// (e.g. `sha256:abc…`), mirroring Subresource-Integrity style. Stored to
/// detect drift; not yet computed or verified by the kernel.
///
/// Serializes as a bare string; **deserialization re-validates** through
/// [`ContentDigest::new`] (via [`TryFrom`]), so a value missing the
/// `algorithm:` prefix cannot enter via the canonical-bytes boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ContentDigest(String);

impl ContentDigest {
    /// Construct a digest, requiring a non-empty `algorithm:value` shape.
    ///
    /// # Errors
    /// Returns [`crate::Error::Invariant`] if `value` is empty or is missing
    /// the `algorithm:` prefix.
    pub fn new(value: impl Into<String>) -> crate::Result<Self> {
        let value = value.into();
        // Require a non-empty algorithm and a non-empty value either side of ':'.
        match value.split_once(':') {
            Some((algo, digest)) if !algo.is_empty() && !digest.is_empty() => Ok(Self(value)),
            _ => Err(crate::Error::Invariant(
                "content digest must be 'algorithm:value'".into(),
            )),
        }
    }

    /// The underlying `algorithm:value` string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<ContentDigest> for String {
    fn from(digest: ContentDigest) -> Self {
        digest.0
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = crate::Error;

    fn try_from(value: String) -> crate::Result<Self> {
        Self::new(value)
    }
}

/// Binds an assertion to a piece of evidence: where it is, and (optionally) a
/// digest of what it was, so later drift is detectable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProvenanceRef {
    /// Where the evidence lives.
    pub uri: Uri,
    /// Optional self-describing digest of the evidence at reference time. A
    /// digest-less ref omits the field entirely from the canonical form (rather
    /// than emitting `null`), keeping the bytes minimal.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<ContentDigest>,
}

impl ProvenanceRef {
    /// A reference with no digest.
    #[must_use]
    pub fn new(uri: Uri) -> Self {
        Self { uri, digest: None }
    }

    /// A reference pinned to a content digest.
    #[must_use]
    pub fn with_digest(uri: Uri, digest: ContentDigest) -> Self {
        Self {
            uri,
            digest: Some(digest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_uri_rejected() {
        assert!(Uri::new("").is_err());
        assert!(Uri::new("file://x").is_ok());
    }

    #[test]
    fn digest_requires_algorithm_prefix() {
        assert!(ContentDigest::new("deadbeef").is_err());
        assert!(ContentDigest::new("sha256:").is_err());
        assert!(ContentDigest::new(":deadbeef").is_err());
        assert!(ContentDigest::new("sha256:deadbeef").is_ok());
    }
}

//! Entry signing (`docs/adr/0033`) — Ed25519 over the canonical bytes.
//!
//! An entry may carry a detached [`Signature`] over its **own canonical bytes
//! with the signature absent** ([`LedgerEntry::signing_bytes`]) — byte-identical
//! to the pre-0033 canonical form, so unsigned history and the golden bytes are
//! unchanged. The verifying [`PublicKey`] is not trusted from the entry itself:
//! the keyring is the **party projection** (the genesis author's key and each
//! `MemberAdded` member's key), and verification is a projection *fact*
//! (`ChannelView::unverified`), never a drop and never a gate — authority stays
//! at the Gate layer (`docs/adr/0004`).
//!
//! Ed25519 because it is what junto's own substrate already signs with (git,
//! SSH); the choice is not load-bearing — any detached signature over the same
//! preimage would do. Secret keys are an **app concern** (machine-local, beside
//! the member codes, never in the ledger); the kernel only wraps the
//! sign/verify mechanism so every caller shares one preimage definition.

use serde::{Deserialize, Serialize};

use crate::{Error, LedgerEntry, Result};

/// The self-describing prefix on the string forms (`ed25519:<hex>`), following
/// the `ContentDigest` `algorithm:value` pattern (`docs/adr/0005`).
const PREFIX: &str = "ed25519:";

/// A member's public verifying key — `ed25519:<64 hex chars>`.
///
/// Published **in the record** (on the genesis author and on `MemberAdded`
/// members) so the party projection doubles as the keyring; validated on
/// deserialize like the other record newtypes (`docs/adr/0008`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PublicKey(String);

impl PublicKey {
    /// Parse and validate the `ed25519:<64 hex>` string form.
    ///
    /// # Errors
    /// Returns [`Error::Invariant`] when the prefix or hex shape is wrong.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let hex = value
            .strip_prefix(PREFIX)
            .ok_or_else(|| Error::Invariant(format!("public key must start with '{PREFIX}'")))?;
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invariant(
                "public key must be 64 hex chars after the prefix".into(),
            ));
        }
        Ok(Self(value))
    }

    /// The full `ed25519:<hex>` string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn to_dalek(&self) -> Result<ed25519_dalek::VerifyingKey> {
        let bytes: [u8; 32] = decode_hex(&self.0[PREFIX.len()..])?
            .try_into()
            .map_err(|_| Error::Invariant("public key must decode to 32 bytes".into()))?;
        ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|e| Error::Invariant(format!("invalid ed25519 public key: {e}")))
    }

    /// Verify a detached signature over arbitrary bytes — the shared
    /// primitive behind [`LedgerEntry::verifies_with`] and any other caller
    /// with its own preimage (e.g. a websocket handshake nonce, an
    /// [`crate::anchor::Annotation`]). Malformed keys/signatures simply fail
    /// to verify rather than erroring, matching `verifies_with`'s
    /// surfaced-fact-not-error stance (`docs/adr/0033`).
    #[must_use]
    pub fn verify_bytes(&self, message: &[u8], signature: &Signature) -> bool {
        let (Ok(key), Ok(sig)) = (self.to_dalek(), signature.to_dalek()) else {
            return false;
        };
        key.verify_strict(message, &sig).is_ok()
    }
}

impl TryFrom<String> for PublicKey {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<PublicKey> for String {
    fn from(key: PublicKey) -> Self {
        key.0
    }
}

/// A detached signature over an entry's [`signing bytes`](LedgerEntry::signing_bytes)
/// — `ed25519:<128 hex chars>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Signature(String);

impl Signature {
    /// Parse and validate the `ed25519:<128 hex>` string form.
    ///
    /// # Errors
    /// Returns [`Error::Invariant`] when the prefix or hex shape is wrong.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let hex = value
            .strip_prefix(PREFIX)
            .ok_or_else(|| Error::Invariant(format!("signature must start with '{PREFIX}'")))?;
        if hex.len() != 128 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invariant(
                "signature must be 128 hex chars after the prefix".into(),
            ));
        }
        Ok(Self(value))
    }

    /// The full `ed25519:<hex>` string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn to_dalek(&self) -> Result<ed25519_dalek::Signature> {
        let bytes: [u8; 64] = decode_hex(&self.0[PREFIX.len()..])?
            .try_into()
            .map_err(|_| Error::Invariant("signature must decode to 64 bytes".into()))?;
        Ok(ed25519_dalek::Signature::from_bytes(&bytes))
    }
}

impl TryFrom<String> for Signature {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Signature> for String {
    fn from(signature: Signature) -> Self {
        signature.0
    }
}

/// A member's secret signing key. **Never serialized, never a ledger entry**
/// (`docs/adr/0033`) — where the secret bytes live (machine-local, beside the
/// member codes) is the app's concern; the kernel only signs with them.
pub struct SigningKey(ed25519_dalek::SigningKey);

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque: a Debug impl must never leak key material.
        f.write_str("SigningKey(..)")
    }
}

impl SigningKey {
    /// Construct from 32 secret bytes (any 32 bytes are a valid Ed25519 seed).
    #[must_use]
    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(&bytes))
    }

    /// Construct from the 64-hex-char secret string form the app stores.
    ///
    /// # Errors
    /// Returns [`Error::Invariant`] when the hex shape is wrong.
    pub fn from_secret_hex(hex: &str) -> Result<Self> {
        let bytes: [u8; 32] = decode_hex(hex)?
            .try_into()
            .map_err(|_| Error::Invariant("secret key must decode to 32 bytes".into()))?;
        Ok(Self::from_secret_bytes(bytes))
    }

    /// The 64-hex-char secret string form (for the app's machine-local store).
    #[must_use]
    pub fn to_secret_hex(&self) -> String {
        encode_hex(&self.0.to_bytes())
    }

    /// The corresponding public verifying key, in record form.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(format!(
            "{PREFIX}{}",
            encode_hex(self.0.verifying_key().as_bytes())
        ))
    }

    /// Sign arbitrary bytes with this key — the shared primitive behind
    /// [`LedgerEntry::sign`] (over an entry's signing bytes) and any other
    /// caller with its own preimage (e.g. a websocket handshake nonce, which
    /// is not a ledger entry).
    #[must_use]
    pub fn sign_bytes(&self, message: &[u8]) -> Signature {
        use ed25519_dalek::Signer as _;
        Signature(format!(
            "{PREFIX}{}",
            encode_hex(&self.0.sign(message).to_bytes())
        ))
    }
}

impl LedgerEntry {
    /// The signature preimage: this entry's canonical bytes **with `signature`
    /// absent** — byte-identical to the pre-0033 canonical form, so the
    /// preimage is exactly what `docs/adr/0008` already pinned. Named here so
    /// no caller re-derives it.
    ///
    /// # Errors
    /// Returns [`Error::Serialization`] if canonicalization fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        if self.signature.is_none() {
            return self.to_canonical_bytes();
        }
        let mut unsigned = self.clone();
        unsigned.signature = None;
        unsigned.to_canonical_bytes()
    }

    /// Sign this entry in place with `key`, replacing any existing signature.
    ///
    /// # Errors
    /// Returns [`Error::Serialization`] if the preimage cannot be produced.
    pub fn sign(&mut self, key: &SigningKey) -> Result<()> {
        self.signature = None;
        self.signature = Some(key.sign_bytes(&self.signing_bytes()?));
        Ok(())
    }

    /// Whether this entry's signature verifies against `key` — the caller
    /// (the projection) supplies the key from the party keyring, never from
    /// the entry itself. Absent or malformed signatures are simply `false`:
    /// verification is a surfaced fact, not an error (`docs/adr/0033`).
    #[must_use]
    pub fn verifies_with(&self, key: &PublicKey) -> bool {
        let Some(signature) = &self.signature else {
            return false;
        };
        let (Ok(verifying), Ok(sig), Ok(bytes)) =
            (key.to_dalek(), signature.to_dalek(), self.signing_bytes())
        else {
            return false;
        };
        verifying.verify_strict(&bytes, &sig).is_ok()
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn decode_hex(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::Invariant("hex string has odd length".into()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Error::Invariant("invalid hex".into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelId, EntryId, EntryPayload, Member, Timestamp};

    fn key_from(seed: u8) -> SigningKey {
        SigningKey::from_secret_bytes([seed; 32])
    }

    fn entry() -> LedgerEntry {
        LedgerEntry {
            signature: None,
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::from_millis(1_700_000_000_000),
            payload: EntryPayload::Assertion {
                statement: "signed".into(),
                rationale: "adr 0033".into(),
                provenance: vec![],
                frame: None,
            },
        }
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let key = key_from(1);
        let mut entry = entry();
        entry.sign(&key).unwrap();
        assert!(entry.verifies_with(&key.public_key()));
    }

    #[test]
    fn tampered_entry_fails_verification() {
        let key = key_from(1);
        let mut entry = entry();
        entry.sign(&key).unwrap();
        entry.author.email = "mallory@example.com".into();
        assert!(!entry.verifies_with(&key.public_key()));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let key = key_from(1);
        let mut entry = entry();
        entry.sign(&key).unwrap();
        assert!(!entry.verifies_with(&key_from(2).public_key()));
    }

    #[test]
    fn unsigned_entry_never_verifies() {
        assert!(!entry().verifies_with(&key_from(1).public_key()));
    }

    #[test]
    fn signing_bytes_match_unsigned_canonical_bytes() {
        let mut signed = entry();
        let unsigned_bytes = signed.to_canonical_bytes().unwrap();
        signed.sign(&key_from(1)).unwrap();
        // The preimage is byte-identical to the pre-signature canonical form.
        assert_eq!(signed.signing_bytes().unwrap(), unsigned_bytes);
        // And the signed form differs (the signature is in the bytes).
        assert_ne!(signed.to_canonical_bytes().unwrap(), unsigned_bytes);
    }

    #[test]
    fn signed_entry_round_trips_through_canonical_bytes() {
        let key = key_from(1);
        let mut entry = entry();
        entry.sign(&key).unwrap();
        let bytes = entry.to_canonical_bytes().unwrap();
        let parsed = LedgerEntry::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(parsed, entry);
        assert!(parsed.verifies_with(&key.public_key()));
    }

    #[test]
    fn newtypes_reject_malformed_values() {
        assert!(PublicKey::new("ed25519:short").is_err());
        assert!(PublicKey::new(format!("rsa:{}", "a".repeat(64))).is_err());
        assert!(Signature::new(format!("ed25519:{}", "a".repeat(127))).is_err());
        assert!(PublicKey::new(format!("ed25519:{}", "a".repeat(64))).is_ok());
        assert!(Signature::new(format!("ed25519:{}", "a".repeat(128))).is_ok());
    }

    #[test]
    fn secret_hex_round_trips() {
        let key = key_from(7);
        let restored = SigningKey::from_secret_hex(&key.to_secret_hex()).unwrap();
        assert_eq!(key.public_key(), restored.public_key());
    }

    #[test]
    fn byte_level_sign_verify_round_trip() {
        let key = SigningKey::from_secret_bytes([3; 32]);
        let sig = key.sign_bytes(b"nonce-bytes");
        assert!(key.public_key().verify_bytes(b"nonce-bytes", &sig));
        assert!(!key.public_key().verify_bytes(b"other", &sig));
    }
}

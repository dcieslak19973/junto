//! `Frame` — the JSON envelope the live websocket speaks.
//!
//! One `LiveDoc` session, one websocket, one `Frame` stream in each
//! direction. Binary payloads (loro update/snapshot bytes, an ephemeral-store
//! update) never ride as raw JSON bytes — they are base64-encoded into a
//! `String` field so the whole frame stays valid JSON end to end.
//!
//! This type is a wire contract shared with the host (`junto serve`, Task 7)
//! and the iced desktop client (Task 9): the variant names, field names, and
//! `#[serde(tag = "t", rename_all = "snake_case")]` shape are load-bearing
//! for both. Do not rename anything here without updating those tasks too.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// One message in the live websocket protocol, in either direction.
///
/// Tagged as `{"t": "<variant>", ...fields}` (`serde`'s internal tagging,
/// `snake_case` variant names) so every frame is self-describing JSON on the
/// wire — no separate framing/length-prefix layer, and clients that only
/// understand a subset of variants can still parse the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Server → client: an authentication challenge. `nonce` is an
    /// ASCII hex-shaped string; the challenged side signs `nonce.as_bytes()`
    /// verbatim (never a decoded byte array — there is nothing to decode).
    Challenge {
        /// The challenge nonce, as sent — never decoded, only re-signed and
        /// compared byte-for-byte.
        nonce: String,
    },
    /// Client → server: the response to a `Challenge`. `signature` is the
    /// sender's detached Ed25519 signature (record string form,
    /// `ed25519:<hex>`) over the challenge nonce's bytes.
    Auth {
        /// The member email the sender claims to be.
        email: String,
        /// Signature over the challenge nonce's bytes, in record string
        /// form.
        signature: String,
    },
    /// Server → client: authentication succeeded; the session may proceed.
    AuthOk,
    /// Either direction: a loro CRDT update or snapshot for the session's
    /// `LiveDoc`, base64-encoded. See [`Frame::update`] and
    /// [`Frame::update_bytes`].
    Update {
        /// Base64 (standard alphabet, padded) encoding of the loro bytes.
        data: String,
    },
    /// Either direction: a loro `EphemeralStore` update (e.g. presence),
    /// base64-encoded. See [`Frame::ephemeral`] and
    /// [`Frame::ephemeral_bytes`].
    Ephemeral {
        /// Base64 (standard alphabet, padded) encoding of the ephemeral
        /// bytes.
        data: String,
    },
    /// Server → client: an `Update` frame was rejected (see
    /// [`crate::validate::validate_annotation_update`]) and never merged
    /// into the session document. `reason` is a human-readable explanation,
    /// not a machine-parsed code.
    Rejected {
        /// Why the frame was rejected.
        reason: String,
    },
    /// Either direction: the session is over; no more frames will follow.
    End,
}

impl Frame {
    /// Build an [`Frame::Update`] frame carrying `bytes` (a loro update or
    /// snapshot), base64-encoded.
    #[must_use]
    pub fn update(bytes: &[u8]) -> Self {
        Self::Update {
            data: STANDARD.encode(bytes),
        }
    }

    /// Decode this frame's payload if it is an [`Frame::Update`].
    ///
    /// Returns `None` both when the variant is not `Update` and when the
    /// `data` field fails to decode as base64 — a malformed frame is a
    /// `None`, not a panic; the caller (the fork-validation gate) treats
    /// either case as "nothing to import".
    #[must_use]
    pub fn update_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Update { data } => STANDARD.decode(data).ok(),
            _ => None,
        }
    }

    /// Build an [`Frame::Ephemeral`] frame carrying `bytes` (an
    /// `EphemeralStore` update), base64-encoded.
    #[must_use]
    pub fn ephemeral(bytes: &[u8]) -> Self {
        Self::Ephemeral {
            data: STANDARD.encode(bytes),
        }
    }

    /// Decode this frame's payload if it is an [`Frame::Ephemeral`]. Same
    /// `None`-on-mismatch-or-bad-base64 contract as
    /// [`Frame::update_bytes`].
    #[must_use]
    pub fn ephemeral_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Ephemeral { data } => STANDARD.decode(data).ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact wire shape is a contract with the host (Task 7) and the
    /// iced client (Task 9): this asserts the literal JSON string, not just
    /// a self round-trip — a round-trip alone would still pass if `t`
    /// became `type`, `rename_all` were dropped, or `data` were renamed.
    #[test]
    fn update_frame_serializes_to_the_documented_wire_shape() {
        let f = Frame::update(b"ab");
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#"{"t":"update","data":"YWI="}"#);
    }

    #[test]
    fn unit_variants_serialize_to_bare_tag_objects() {
        assert_eq!(
            serde_json::to_string(&Frame::AuthOk).unwrap(),
            r#"{"t":"auth_ok"}"#
        );
        assert_eq!(
            serde_json::to_string(&Frame::End).unwrap(),
            r#"{"t":"end"}"#
        );
    }

    #[test]
    fn frame_update_round_trips_base64() {
        let f = Frame::update(b"\x00\x01binary");
        let json = serde_json::to_string(&f).unwrap();
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.update_bytes().unwrap(), b"\x00\x01binary");
    }

    #[test]
    fn frame_ephemeral_round_trips_base64() {
        let f = Frame::ephemeral(b"\x02presence");
        let json = serde_json::to_string(&f).unwrap();
        let back: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ephemeral_bytes().unwrap(), b"\x02presence");
    }

    #[test]
    fn bytes_helpers_are_none_for_the_wrong_variant() {
        assert_eq!(Frame::AuthOk.update_bytes(), None);
        assert_eq!(Frame::End.ephemeral_bytes(), None);
    }

    #[test]
    fn malformed_base64_decodes_to_none_not_a_panic() {
        let update = Frame::Update {
            data: "not valid base64 !!!".to_string(),
        };
        assert_eq!(update.update_bytes(), None);
        let ephemeral = Frame::Ephemeral {
            data: "@@@".to_string(),
        };
        assert_eq!(ephemeral.ephemeral_bytes(), None);
    }
}

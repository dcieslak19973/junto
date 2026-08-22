//! The enrollment envelope: `junto://invite?code=…` and
//! `junto://enroll?code=…` URIs (`docs/adr` device-key-enrollment plan).
//!
//! Two codes carry a new device into a channel:
//!
//! - **invite**, minted by a founder (`junto invite`): a bearer grant that
//!   `member_email` may join every channel in `channels`, proven by
//!   presenting [`InvitePayload::invite_token`]. The token is a secret —
//!   anyone holding
//!   the decoded payload can redeem the grant it names (`invites::consume`
//!   is the redemption check) — so this code must be delivered only to the
//!   intended member.
//! - **enroll**, minted by the new device (`junto enroll --invite …`): the
//!   device's freshly generated public key, echoed back alongside the
//!   invite token that proves which grant it is answering. Unlike the
//!   invite, this carries **no secret** — a public key grants nothing by
//!   itself; authority comes entirely from the founder's own act of
//!   recording it on the ledger (`junto add-member`). So the enroll code is
//!   safe to paste into a chat, read aloud, or leave in shell history.
//!
//! Both shapes are modelled on Orca's shipped `orca environment add
//! --pairing-code` envelope, read out of the app rather than guessed: a
//! versioned JSON payload, base64url-encoded into a URI, every field
//! length-bounded, a total size cap enforced *before* any parsing, and a
//! short TTL with an explicit clock-skew allowance (two machines' clocks
//! differ). Decoding validates in a fixed order — see [`decode_invite`] and
//! [`decode_enroll`] — so an oversized or malformed code is rejected as
//! cheaply as possible, before the bytes it might carry are ever parsed.
//!
//! `junto invite` (Task 6), `junto enroll` (Task 7), and
//! `junto add-member --enroll` (Task 8) now call into this module — every
//! private helper and constant is reachable from a wired entry point, so
//! nothing here carries `#[allow(dead_code)]` any more.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use junto_kernel::{PublicKey, Timestamp};
use serde::{Deserialize, Serialize};

/// Maximum lifetime of an invite or enroll code, in milliseconds (10
/// minutes) — Orca's shipped pairing-offer envelope's TTL, reused as-is
/// rather than invented for junto. Long enough for a human to relay a code
/// (paste it into chat, read it aloud) but short enough that a leaked or
/// forgotten code stops mattering quickly.
pub const MAX_INVITE_TTL_MS: i64 = 600_000;

/// Allowance for clock skew between the two machines exchanging a code, in
/// milliseconds. An `expires_at` up to this far in the past (issuer's clock
/// behind) is still accepted, and the upper bound on how far in the future
/// `expires_at` may legally sit is `MAX_INVITE_TTL_MS` **plus** this
/// allowance (issuer's clock ahead). Same value as Orca's envelope.
pub const EXPIRY_CLOCK_SKEW_MS: i64 = 30_000;

/// Hard cap on a code's total character count (the whole `junto://…` URI),
/// checked **before** any base64 or JSON parsing is attempted — an
/// oversized string is rejected on `len()` alone, so a malicious or
/// corrupted code cannot spend CPU decoding or allocating arbitrarily large
/// input. Orca's own bound, generously above the largest legitimate
/// payload (every string field at [`MAX_FIELD_CHARS`], base64-expanded).
pub const MAX_CODE_CHARS: usize = 132_096;

/// Hard cap on any single string field inside a decoded payload, checked
/// after JSON parsing. Bounds the cost of holding or echoing a field back
/// to a human and catches a payload that parses cleanly but carries absurd
/// content (e.g. a `channel` name someone pasted a book into).
pub const MAX_FIELD_CHARS: usize = 4_096;

/// Maximum number of channels a single invite may name — a founder
/// ticking more than 32 channels in one pass is a mistake, and an
/// unbounded set makes redemption fan out unboundedly.
pub const MAX_INVITE_CHANNELS: usize = 32;

/// The current version this crate produces and accepts. `decode_invite` and
/// `decode_enroll` reject any other value in a payload's `v` field, so a
/// future format change can be introduced without a mis-parsed old payload
/// silently passing as valid.
pub(crate) const PAYLOAD_VERSION: u8 = 2;

/// The payload behind a `junto://invite?code=…` URI — see the module docs
/// for what it grants and why [`invite_token`](Self::invite_token) is a
/// secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitePayload {
    pub v: u8,
    pub invite_token: String,
    pub member_email: String,
    #[serde(default)]
    pub channels: Vec<String>,
    pub expires_at: i64,
}

/// The payload behind a `junto://enroll?code=…` URI — see the module docs
/// for why it carries no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollPayload {
    pub v: u8,
    pub invite_token: String,
    pub email: String,
    pub display_name: String,
    pub public_key: PublicKey,
    pub expires_at: i64,
}

/// Encode `payload` as a `junto://invite?code=<base64url>` URI.
///
/// # Errors
/// Returns an error if `payload` cannot be serialized to JSON (not expected
/// for this type; surfaced rather than panicking).
pub fn encode_invite(payload: &InvitePayload) -> Result<String> {
    encode("invite", payload)
}

/// Encode `payload` as a `junto://enroll?code=<base64url>` URI.
///
/// # Errors
/// Returns an error if `payload` cannot be serialized to JSON (not expected
/// for this type; surfaced rather than panicking).
pub fn encode_enroll(payload: &EnrollPayload) -> Result<String> {
    encode("enroll", payload)
}

fn encode(host: &str, payload: &(impl Serialize + ?Sized)) -> Result<String> {
    let json = serde_json::to_string(payload).context("serializing enrollment payload")?;
    Ok(format!(
        "junto://{host}?code={}",
        URL_SAFE_NO_PAD.encode(json)
    ))
}

/// Decode and validate a `junto://invite?code=…` URI. See the module docs
/// for the validation order.
///
/// # Errors
/// Returns an error for an oversized, malformed, wrong-version, out-of-
/// bounds, or expired code.
pub fn decode_invite(url: &str) -> Result<InvitePayload> {
    let json = decode_code(url, "invite")?;
    let payload: InvitePayload =
        serde_json::from_str(&json).context("parsing invite payload JSON")?;
    check_version(payload.v)?;
    check_channel_set(&payload.channels)?;
    check_field_bounds(
        [
            ("invite_token", payload.invite_token.as_str()),
            ("member_email", payload.member_email.as_str()),
        ]
        .into_iter()
        .chain(payload.channels.iter().map(|c| ("channel", c.as_str()))),
    )?;
    check_expiry(payload.expires_at)?;
    Ok(payload)
}

/// Decode and validate a `junto://enroll?code=…` URI. See the module docs
/// for the validation order.
///
/// # Errors
/// Returns an error for an oversized, malformed, wrong-version, out-of-
/// bounds, or expired code.
pub fn decode_enroll(url: &str) -> Result<EnrollPayload> {
    let json = decode_code(url, "enroll")?;
    let payload: EnrollPayload =
        serde_json::from_str(&json).context("parsing enroll payload JSON")?;
    check_version(payload.v)?;
    check_field_bounds([
        ("invite_token", payload.invite_token.as_str()),
        ("email", payload.email.as_str()),
        ("display_name", payload.display_name.as_str()),
    ])?;
    check_expiry(payload.expires_at)?;
    Ok(payload)
}

/// Strip the `junto://<host>?code=` wrapper and base64url-decode the code,
/// enforcing the total-length cap first — before either the scheme/host
/// check or any decoding touches the string.
fn decode_code(url: &str, host: &str) -> Result<String> {
    if url.len() > MAX_CODE_CHARS {
        bail!("code exceeds the {MAX_CODE_CHARS}-char limit");
    }
    let prefix = format!("junto://{host}?code=");
    let code = url
        .strip_prefix(&prefix)
        .with_context(|| format!("expected a junto://{host}?code=… URI"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(code)
        .context("code is not valid base64url")?;
    String::from_utf8(bytes).context("decoded code is not valid UTF-8")
}

fn check_version(v: u8) -> Result<()> {
    if v != PAYLOAD_VERSION {
        bail!(
            "unsupported payload version {v} (expected {PAYLOAD_VERSION}) — this code came \
             from an older junto; mint a new one"
        );
    }
    Ok(())
}

/// Refuse an invite naming no channels — redemption would burn a token
/// and grant nothing — or naming more than `MAX_INVITE_CHANNELS`. Each
/// channel's own length is bounded separately, by `check_field_bounds`
/// alongside `invite_token` and `member_email`.
fn check_channel_set(channels: &[String]) -> Result<()> {
    if channels.is_empty() {
        bail!("an invite must name at least one channel");
    }
    if channels.len() > MAX_INVITE_CHANNELS {
        bail!("an invite may name at most {MAX_INVITE_CHANNELS} channels");
    }
    Ok(())
}

fn check_field_bounds<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<()> {
    for (name, value) in fields {
        if value.chars().count() > MAX_FIELD_CHARS {
            bail!("field '{name}' exceeds the {MAX_FIELD_CHARS}-char limit");
        }
    }
    Ok(())
}

/// Refuse an `expires_at` already past beyond the skew allowance, or one
/// further ahead than `MAX_INVITE_TTL_MS` plus skew — a code claiming a
/// year of validity is as wrong as an already-expired one.
fn check_expiry(expires_at: i64) -> Result<()> {
    let now = Timestamp::now().as_millis();
    if expires_at < now - EXPIRY_CLOCK_SKEW_MS {
        bail!("code has expired");
    }
    if expires_at > now + MAX_INVITE_TTL_MS + EXPIRY_CLOCK_SKEW_MS {
        bail!("code's expiry is further out than the maximum TTL allows");
    }
    Ok(())
}

/// Mint a fresh invite token: 32 bytes of entropy (two v4 UUIDs — the
/// crate's existing entropy source, `keys.rs`/`members.rs`) as 43 base64url
/// characters (no padding). This IS the bearer secret an invite proves
/// possession of, unlike the enroll payload's public key.
#[must_use]
pub fn mint_invite_token() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_invite() -> InvitePayload {
        InvitePayload {
            v: PAYLOAD_VERSION,
            invite_token: mint_invite_token(),
            member_email: "dan@example.com".to_string(),
            channels: vec!["junto-dev".to_string()],
            expires_at: Timestamp::now().as_millis() + 60_000,
        }
    }

    fn sample_enroll() -> EnrollPayload {
        EnrollPayload {
            v: PAYLOAD_VERSION,
            invite_token: mint_invite_token(),
            email: "dan@example.com".to_string(),
            display_name: "Dan's Laptop".to_string(),
            public_key: PublicKey::new(format!("ed25519:{}", "a".repeat(64))).unwrap(),
            expires_at: Timestamp::now().as_millis() + 60_000,
        }
    }

    #[test]
    fn invite_round_trips_through_its_uri() {
        let p = sample_invite();
        let url = encode_invite(&p).unwrap();
        assert!(url.starts_with("junto://invite?code="));
        assert_eq!(decode_invite(&url).unwrap(), p);
    }

    #[test]
    fn enroll_round_trips_and_carries_no_secret() {
        let p = sample_enroll();
        let url = encode_enroll(&p).unwrap();
        assert!(url.starts_with("junto://enroll?code="));
        assert_eq!(decode_enroll(&url).unwrap(), p);
        // `EnrollPayload` has no secret-key field to begin with — only a
        // `PublicKey` — so there is nothing here for a leak to expose; the
        // type makes "safe to paste or read aloud" true by construction,
        // not by convention.
    }

    #[test]
    fn an_oversized_code_is_refused_before_parsing() {
        let url = format!("junto://invite?code={}", "A".repeat(MAX_CODE_CHARS + 1));
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn an_expired_invite_is_refused() {
        let mut p = sample_invite();
        p.expires_at = Timestamp::now().as_millis() - MAX_INVITE_TTL_MS;
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn an_invite_inside_the_skew_allowance_is_accepted() {
        // expires_at a few seconds in the past, within EXPIRY_CLOCK_SKEW_MS → Ok.
        // Two machines' clocks differ; this is why the allowance exists.
        let mut p = sample_invite();
        p.expires_at = Timestamp::now().as_millis() - (EXPIRY_CLOCK_SKEW_MS / 2);
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_ok());
    }

    #[test]
    fn an_invite_expiring_beyond_the_ttl_is_refused() {
        let mut p = sample_invite();
        p.expires_at = Timestamp::now().as_millis() + 2 * MAX_INVITE_TTL_MS;
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn a_wrong_version_is_refused() {
        let mut p = sample_invite();
        p.v = PAYLOAD_VERSION + 1;
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn a_minted_token_is_43_base64url_chars() {
        let t = mint_invite_token();
        assert_eq!(t.len(), 43);
        assert!(
            t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn a_field_beyond_the_bound_is_refused() {
        let mut p = sample_invite();
        p.channels = vec!["x".repeat(MAX_FIELD_CHARS + 1)];
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn decode_invite_rejects_an_enroll_coded_url() {
        // Assert the error names the expected host, not merely that
        // decoding failed — a wrong-shaped-payload error from serde would
        // also make `decode_invite` fail here, without the host check
        // being what actually fired.
        let url = encode_enroll(&sample_enroll()).unwrap();
        let err = decode_invite(&url).unwrap_err();
        assert!(err.to_string().contains("junto://invite"));
    }

    #[test]
    fn decode_enroll_rejects_an_invite_coded_url() {
        let url = encode_invite(&sample_invite()).unwrap();
        let err = decode_enroll(&url).unwrap_err();
        assert!(err.to_string().contains("junto://enroll"));
    }

    #[test]
    fn decode_invite_rejects_a_url_missing_the_code_param() {
        assert!(decode_invite("junto://invite").is_err());
    }

    #[test]
    fn decode_invite_rejects_a_foreign_scheme() {
        assert!(decode_invite("https://invite?code=abc").is_err());
    }

    #[test]
    fn the_length_cap_is_checked_before_the_scheme_and_host() {
        // Oversized AND not even shaped like a `junto://invite?code=…` URI:
        // if the prefix check ran first, this would fail with "expected a
        // junto://…" instead — pins that length is checked before the
        // scheme/host check, not merely before base64/JSON.
        let url = "z".repeat(MAX_CODE_CHARS + 1);
        let err = decode_invite(&url).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn the_total_cap_is_enforced_before_the_json_parser_ever_runs() {
        // A code this large would also fail JSON/UTF-8 parsing on its own, but
        // the point pinned here is *which* check fires: the oversized-length
        // error message, not a parser error, proving length is checked first.
        let url = format!("junto://invite?code={}", "A".repeat(MAX_CODE_CHARS + 1));
        let err = decode_invite(&url).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn a_code_at_exactly_the_max_length_is_not_rejected_for_length() {
        // Same shape as the oversized case, one char shorter: MUST NOT be
        // rejected by the length check (it will still fail to parse as
        // valid base64/JSON, but for a different reason) — pins `>` rather
        // than `>=` in the length bound.
        let prefix = "junto://invite?code=";
        let url = format!("{prefix}{}", "A".repeat(MAX_CODE_CHARS - prefix.len()));
        assert_eq!(url.len(), MAX_CODE_CHARS);
        let err = decode_invite(&url).unwrap_err();
        assert!(!err.to_string().contains("exceeds"));
    }

    #[test]
    fn a_v2_invite_round_trips_a_multi_channel_set() {
        let mut p = sample_invite();
        p.channels = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let url = encode_invite(&p).unwrap();
        assert!(url.starts_with("junto://invite?code="));
        assert_eq!(
            decode_invite(&url).unwrap(),
            p,
            "the whole set survives the round trip"
        );
    }

    #[test]
    fn an_empty_channel_set_is_refused() {
        // An invite that grants nothing is a bug in the caller, not a valid code:
        // redemption would burn a token and append nothing.
        let mut p = sample_invite();
        p.channels = Vec::new();
        let url = encode_invite(&p).unwrap();
        let err = decode_invite(&url).unwrap_err().to_string();
        assert!(err.contains("at least one channel"), "{err}");
    }

    #[test]
    fn a_channel_set_beyond_the_cap_is_refused() {
        let mut p = sample_invite();
        p.channels = (0..MAX_INVITE_CHANNELS + 1)
            .map(|i| format!("c{i}"))
            .collect();
        let url = encode_invite(&p).unwrap();
        let err = decode_invite(&url).unwrap_err().to_string();
        assert!(err.contains(&MAX_INVITE_CHANNELS.to_string()), "{err}");
    }

    #[test]
    fn a_channel_set_at_exactly_the_cap_is_not_rejected_for_count() {
        // Same shape as the over-the-cap case, one element fewer: MUST NOT
        // be rejected by the count check — pins `>` rather than `>=` in
        // the cap comparison.
        let mut p = sample_invite();
        p.channels = (0..MAX_INVITE_CHANNELS).map(|i| format!("c{i}")).collect();
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_ok());
    }

    #[test]
    fn a_channel_name_beyond_the_field_bound_is_refused() {
        // The per-element bound, not the set bound: one absurd element in an
        // otherwise sane set. A mutation that only checks the Vec length passes
        // the test above and fails this one.
        let mut p = sample_invite();
        p.channels = vec!["fine".to_string(), "x".repeat(MAX_FIELD_CHARS + 1)];
        let url = encode_invite(&p).unwrap();
        assert!(decode_invite(&url).is_err());
    }

    #[test]
    fn a_v1_invite_is_refused_with_instructions_to_mint_a_new_one() {
        // Hand-build a v1 body: the struct can no longer express it.
        let body = r#"{"v":1,"invite_token":"t","member_email":"dan@x.com","channel":"junto-dev","expires_at":0}"#;
        let url = format!(
            "junto://invite?code={}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)
        );
        let err = decode_invite(&url).unwrap_err().to_string();
        assert!(err.contains("mint a new one"), "{err}");
    }
}

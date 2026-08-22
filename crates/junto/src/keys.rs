//! The machine-local signing-key store (`docs/adr/0033`).
//!
//! Every member who writes through this host — human or agent — gets an
//! **Ed25519 keypair of their own**: an agent's key is minted like its member
//! code and is never its operator's, so the human/agent split stays legible at
//! the byte level. Secrets live in `<junto-home>/keys.toml`, beside the member
//! codes (`docs/adr/0017`), and **never in the ledger** — only the public key
//! enters the record (on the membership-granting entries), where the party
//! projection reads it as the channel keyring.
//!
//! Honest threat model, same as `members.rs`: one machine, one OS user; any
//! local process can read this file. The signature's value is downstream —
//! after `docs/adr/0011` sync, an entry's authorship is verifiable against the
//! keyring instead of being an unauthenticated string in a per-author ref.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::SigningKey;
use serde::{Deserialize, Serialize};

/// One stored keypair: the member identity (by email) and the secret seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRecord {
    email: String,
    /// 64 hex chars — the Ed25519 secret seed. Machine-local; never synced.
    secret: String,
    /// 64 hex chars — the Ed25519 seed for this device's transport key
    /// (`docs/adr/0033` two-key separation), kept beside `secret` rather
    /// than replacing it: minting this must never rotate `secret`, or
    /// every entry this device ever signed would flip to `unverified`.
    /// Absent on a record written before this field existed; `transport_key`
    /// mints one on first use and writes it back beside the untouched
    /// signing secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport_secret: Option<String>,
    /// Whether this host minted `secret` for an identity it already had
    /// authority over — an agent it created, or a human already speaking
    /// through it (`sign_entry`'s own mint-on-first-use, or a direct
    /// [`signing_key`] call) — as opposed to [`enrolled_signing_key`]'s
    /// mint, which mints for whatever email a DECODED, UNVALIDATED invite
    /// payload names (finding 9a, final fix wave): `/devices/enroll` and
    /// `junto enroll` deliberately have no founder check, so a key they
    /// mint alone must never authorize `Host::add_member`'s keyless-human
    /// reuse path the way an authored key does. `#[serde(default = …)]`
    /// so every record written before this field existed reads as
    /// authored — the meaning `signing_key`'s mint always had.
    #[serde(default = "authored_default")]
    authored: bool,
}

fn authored_default() -> bool {
    true
}

/// The serialized shape of `<junto-home>/keys.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct KeysFile {
    #[serde(default)]
    keys: Vec<KeyRecord>,
}

fn keys_path(junto_home: &Path) -> PathBuf {
    junto_home.join("keys.toml")
}

fn load(junto_home: &Path) -> Result<KeysFile> {
    let path = keys_path(junto_home);
    if !path.exists() {
        return Ok(KeysFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The signing key for `email`, minting one on first use (mirroring the
/// member-code mint: per identity per machine, reused across channels).
/// Every caller that reaches this already resolved a LOCAL identity this
/// host has authority over — an agent it created, or a human logged in
/// through it (`Host::sign_entry`'s own mint-on-first-use) — so a fresh
/// mint is recorded `authored: true`. `/devices/enroll` and `junto
/// enroll` must call [`enrolled_signing_key`] instead (finding 9a).
pub fn signing_key(junto_home: &Path, email: &str) -> Result<SigningKey> {
    mint_or_reuse(junto_home, email, true)
}

/// The signing key for `email` from a decoded invite/enroll payload —
/// `/devices/enroll` and `junto enroll`'s own local mint. These endpoints
/// have no founder check and no member code by design (the invite token
/// is the eventual grant's authorization, checked at redemption); the
/// email minted here comes from whatever a composed invite claims, never
/// from a resolved local identity. A fresh mint is recorded
/// `authored: false` so it can never, by its mere existence, satisfy
/// `Host::add_member`'s keyless-human reuse check (finding 9a, final fix
/// wave). Reusing an EXISTING record (of either provenance) is
/// unaffected — this never downgrades a key `signing_key` already
/// authored.
pub fn enrolled_signing_key(junto_home: &Path, email: &str) -> Result<SigningKey> {
    mint_or_reuse(junto_home, email, false)
}

fn mint_or_reuse(junto_home: &Path, email: &str, authored: bool) -> Result<SigningKey> {
    let mut file = load(junto_home)?;
    if let Some(record) = file.keys.iter().find(|record| record.email == email) {
        return SigningKey::from_secret_hex(&record.secret)
            .with_context(|| format!("keys.toml holds a malformed secret for {email}"));
    }

    // Mint: 32 fresh random bytes are a valid Ed25519 seed. Entropy from the
    // OS via two v4 UUIDs (the crate's existing source — this *is* key
    // material, and uuid v4 draws from the OS CSPRNG).
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let key = SigningKey::from_secret_bytes(seed);
    file.keys.push(KeyRecord {
        email: email.to_string(),
        secret: key.to_secret_hex(),
        transport_secret: None,
        authored,
    });
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        keys_path(junto_home),
        toml::to_string_pretty(&file).context("serializing key store")?,
    )
    .with_context(|| format!("writing {}", keys_path(junto_home).display()))?;
    Ok(key)
}

/// Whether `email` already has a signing key on file — **never mints**
/// one, unlike [`signing_key`]. [`signing_key`] mints because every
/// caller that reaches it already resolved a LOCAL identity this host has
/// authority over: an agent it created, or a human logged in through it.
/// This function exists for callers handed an identity from elsewhere
/// (e.g. a remote watcher's [`junto_kernel::Member`] over
/// `crate::live_bridge`), which must be able to ask "do I already hold a
/// key for this person" without the asking itself silently minting and
/// persisting a new keypair for someone this host has no authority to
/// speak for. Answers regardless of provenance — `has_authored_signing_key`
/// is the narrower question `Host::add_member`'s authorization needs.
pub fn has_signing_key(junto_home: &Path, email: &str) -> Result<bool> {
    Ok(load(junto_home)?
        .keys
        .iter()
        .any(|record| record.email == email))
}

/// Whether `email` has a signing key this host minted for an identity it
/// already had authority over (finding 9a, final fix wave) —
/// `Host::add_member`'s keyless-human path's ONLY legitimate reuse
/// signal. `has_signing_key` answers a different question (any key on
/// file, regardless of how it got there) for `live_bridge`'s remote-
/// watcher check, which this must never replace.
pub fn has_authored_signing_key(junto_home: &Path, email: &str) -> Result<bool> {
    Ok(load(junto_home)?
        .keys
        .iter()
        .any(|record| record.email == email && record.authored))
}

/// The transport key for `email`, minting one on first use — mirrors
/// [`signing_key`], but writes into [`KeyRecord::transport_secret`], **beside**
/// the (possibly already-present) signing `secret`, never touching it. A
/// device that re-enrolls keeps signing with the key its past entries were
/// signed by; rotating it here would flip every one of those entries to
/// `unverified`.
pub fn transport_key(junto_home: &Path, email: &str) -> Result<SigningKey> {
    // Ensure a record exists for this identity first — reuses
    // `signing_key`'s own idempotent mint-on-first-use, and crucially never
    // touches an existing `secret` (it returns the existing one untouched).
    signing_key(junto_home, email)?;

    let mut file = load(junto_home)?;
    let record = file
        .keys
        .iter_mut()
        .find(|record| record.email == email)
        .expect("signing_key just ensured a record exists for this email");
    if let Some(secret) = &record.transport_secret {
        return SigningKey::from_secret_hex(secret)
            .with_context(|| format!("keys.toml holds a malformed transport secret for {email}"));
    }

    // Mint: same entropy source as `signing_key` — a genuinely distinct
    // keypair, never a copy of the signing key.
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let key = SigningKey::from_secret_bytes(seed);
    record.transport_secret = Some(key.to_secret_hex());
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        keys_path(junto_home),
        toml::to_string_pretty(&file).context("serializing key store")?,
    )
    .with_context(|| format!("writing {}", keys_path(junto_home).display()))?;
    Ok(key)
}

/// Whether `email` already has a transport key on file — **never mints**
/// one, mirroring [`has_signing_key`] for the same reason. Exercised by
/// `host::lineage_tests::add_member_keyless_still_mints_for_a_local_agent`,
/// but has no caller from non-test code yet — `Host::add_member`'s
/// local-mint path mints unconditionally, mirroring `keyed`'s own
/// signing-key mint, which never consults `has_signing_key` either. A
/// later transport-slice task (the `keys.json`/`/devices/enroll`
/// endpoints) is this function's first real *production* caller, matching
/// `enroll.rs`'s own precedent for kernel API landed ahead of its wiring.
#[allow(dead_code)]
pub fn has_transport_key(junto_home: &Path, email: &str) -> Result<bool> {
    Ok(load(junto_home)?
        .keys
        .iter()
        .any(|record| record.email == email && record.transport_secret.is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minting_is_idempotent_per_identity() {
        let home = tempfile::tempdir().unwrap();
        let first = signing_key(home.path(), "dan@example.com").unwrap();
        let second = signing_key(home.path(), "dan@example.com").unwrap();
        assert_eq!(first.public_key(), second.public_key());
    }

    #[test]
    fn distinct_identities_get_distinct_keys() {
        let home = tempfile::tempdir().unwrap();
        let dan = signing_key(home.path(), "dan@example.com").unwrap();
        let agent = signing_key(home.path(), "agent@agents.junto").unwrap();
        assert_ne!(dan.public_key(), agent.public_key());
    }

    #[test]
    fn secret_never_leaves_the_home() {
        let home = tempfile::tempdir().unwrap();
        let key = signing_key(home.path(), "dan@example.com").unwrap();
        let stored = std::fs::read_to_string(keys_path(home.path())).unwrap();
        assert!(stored.contains(&key.to_secret_hex()));
        // The public form is derivable, not stored — nothing else to sync.
        assert!(!stored.contains(key.public_key().as_str()));
    }

    #[test]
    fn has_signing_key_never_mints() {
        let home = tempfile::tempdir().unwrap();
        assert!(!has_signing_key(home.path(), "unknown@elsewhere.com").unwrap());
        // The lookup itself must not have minted or written anything.
        assert!(!keys_path(home.path()).exists());
        assert!(load(home.path()).unwrap().keys.is_empty());

        signing_key(home.path(), "dan@example.com").unwrap();
        assert!(has_signing_key(home.path(), "dan@example.com").unwrap());
        assert!(!has_signing_key(home.path(), "unknown@elsewhere.com").unwrap());
    }

    #[test]
    fn a_device_holds_two_distinct_keys() {
        let home = tempfile::tempdir().unwrap();
        let signing = signing_key(home.path(), "dan@x.com").unwrap();
        let transport = transport_key(home.path(), "dan@x.com").unwrap();
        assert_ne!(signing.public_key(), transport.public_key());
    }

    #[test]
    fn minting_a_transport_key_leaves_an_existing_signing_key_untouched() {
        // The rotation trap: a keys.toml written before this change has a secret
        // and no transport_secret. Minting the transport half must not touch the
        // signing half, or every entry that device ever signed goes unverified.
        let home = tempfile::tempdir().unwrap();
        let before = signing_key(home.path(), "dan@x.com")
            .unwrap()
            .to_secret_hex();
        let _ = transport_key(home.path(), "dan@x.com").unwrap();
        assert_eq!(
            signing_key(home.path(), "dan@x.com")
                .unwrap()
                .to_secret_hex(),
            before
        );
    }

    #[test]
    fn has_transport_key_is_false_before_minting_and_true_after() {
        let home = tempfile::tempdir().unwrap();
        signing_key(home.path(), "dan@x.com").unwrap();
        assert!(!has_transport_key(home.path(), "dan@x.com").unwrap());
        transport_key(home.path(), "dan@x.com").unwrap();
        assert!(has_transport_key(home.path(), "dan@x.com").unwrap());
    }

    /// Finding 9a (final fix wave): a record `signing_key` (or its own
    /// mint-on-first-use elsewhere) minted is authored; `has_authored_
    /// signing_key` must see it even though `has_signing_key` also does.
    #[test]
    fn has_authored_signing_key_is_true_for_a_key_signing_key_minted() {
        let home = tempfile::tempdir().unwrap();
        signing_key(home.path(), "dan@example.com").unwrap();
        assert!(has_authored_signing_key(home.path(), "dan@example.com").unwrap());
    }

    /// The core of finding 9a: a key `enrolled_signing_key` mints (what
    /// `/devices/enroll`/`junto enroll` call, for whatever email a
    /// decoded, unvalidated invite names) exists — `has_signing_key`
    /// still sees it — but is NOT authored, so `Host::add_member`'s
    /// keyless-human reuse check (which now calls
    /// `has_authored_signing_key`) cannot be pre-satisfied by it.
    #[test]
    fn has_authored_signing_key_is_false_for_a_key_enrolled_signing_key_minted() {
        let home = tempfile::tempdir().unwrap();
        enrolled_signing_key(home.path(), "victim@example.com").unwrap();
        assert!(has_signing_key(home.path(), "victim@example.com").unwrap());
        assert!(!has_authored_signing_key(home.path(), "victim@example.com").unwrap());
    }

    /// `enrolled_signing_key` reuses an EXISTING record's key verbatim —
    /// never downgrades a key `signing_key` already authored, and never
    /// rotates the secret.
    #[test]
    fn enrolled_signing_key_reuses_an_already_authored_key_without_downgrading_it() {
        let home = tempfile::tempdir().unwrap();
        let authored = signing_key(home.path(), "dan@example.com").unwrap();
        let reused = enrolled_signing_key(home.path(), "dan@example.com").unwrap();
        assert_eq!(authored.public_key(), reused.public_key());
        assert!(has_authored_signing_key(home.path(), "dan@example.com").unwrap());
    }

    /// A `keys.toml` written before the `authored` field existed must
    /// read every record as authored — the meaning `signing_key`'s mint
    /// always had, and the only way an upgrade does not silently break
    /// `Host::add_member`'s existing local-identity reuse for pre-fix
    /// installs.
    #[test]
    fn a_pre_existing_record_with_no_authored_field_reads_as_authored() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            keys_path(home.path()),
            "[[keys]]\nemail = \"dan@example.com\"\nsecret = \"a\"\n",
        )
        .unwrap();
        assert!(has_authored_signing_key(home.path(), "dan@example.com").unwrap());
    }
}

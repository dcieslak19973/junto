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
pub fn signing_key(junto_home: &Path, email: &str) -> Result<SigningKey> {
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
}

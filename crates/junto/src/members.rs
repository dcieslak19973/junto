//! The machine-local member-code store (`docs/adr/0017`).
//!
//! Minting a member also mints a **member code** — a random 6-character
//! alphanumeric secret tied to that member identity. Codes live in
//! `<junto-home>/members.toml`, beside the substrate registry, and **never in
//! the ledger**: the record syncs to remotes, so a secret in an entry would be
//! no secret. The host's write surfaces check the code; the projection cannot
//! and does not (entries arriving by sync carry none).
//!
//! Honest threat model: accident-proofing, not security. Everything here is
//! one machine, one OS user; any local process can read this file (which is
//! also why the codes are stored in plaintext — hashing would add ceremony,
//! not safety, at this scope). What it buys: an agent can no longer author as
//! its operator (or as another agent) by *mistake*.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::Member;
use serde::{Deserialize, Serialize};

/// One minted member: the identity plus its machine-local code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRecord {
    #[serde(flatten)]
    pub member: Member,
    pub code: String,
}

/// The serialized shape of `<junto-home>/members.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct MembersFile {
    #[serde(default)]
    members: Vec<MemberRecord>,
}

/// The outcome of minting: the member's code, and whether it was created by
/// this call (so callers can print it once) or already existed.
#[derive(Debug)]
pub struct Minted {
    pub code: String,
    pub newly_minted: bool,
}

/// The outcome of checking a presented code against the store.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeCheck {
    /// The code matches the one on file.
    Valid,
    /// A code is on file for this identity, but the presented one differs.
    WrongCode,
    /// This identity was never minted on this machine.
    NoCodeOnFile,
}

fn members_path(junto_home: &Path) -> PathBuf {
    junto_home.join("members.toml")
}

/// Every minted member. A missing file is an empty store, not an error.
pub fn minted_members(junto_home: &Path) -> Result<Vec<MemberRecord>> {
    let path = members_path(junto_home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let file: MembersFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(file.members)
}

/// Mint a code for `member` — or return the existing one (the code is per
/// member identity per machine, reused across channels; `docs/adr/0017`).
pub fn mint(junto_home: &Path, member: &Member) -> Result<Minted> {
    let mut members = minted_members(junto_home)?;
    if let Some(existing) = members
        .iter()
        .find(|record| record.member.email == member.email)
    {
        return Ok(Minted {
            code: existing.code.clone(),
            newly_minted: false,
        });
    }

    let code = generate_code();
    members.push(MemberRecord {
        member: member.clone(),
        code: code.clone(),
    });
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    let file = MembersFile { members };
    std::fs::write(
        members_path(junto_home),
        toml::to_string_pretty(&file).context("serializing member store")?,
    )
    .with_context(|| format!("writing {}", members_path(junto_home).display()))?;
    Ok(Minted {
        code,
        newly_minted: true,
    })
}

/// Check a presented code against the store.
pub fn check(junto_home: &Path, email: &str, presented: &str) -> Result<CodeCheck> {
    let members = minted_members(junto_home)?;
    Ok(
        match members.iter().find(|record| record.member.email == email) {
            None => CodeCheck::NoCodeOnFile,
            Some(record) if record.code == presented => CodeCheck::Valid,
            Some(_) => CodeCheck::WrongCode,
        },
    )
}

/// A random 6-character alphanumeric code. Randomness comes from a v4 UUID
/// (the crate's existing entropy source); the modulo over 62 introduces a
/// negligible bias, which is fine for accident-proofing (`docs/adr/0017` —
/// this is not key material).
fn generate_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(6)
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dan() -> Member {
        Member::human("Dan", "dan@example.com")
    }

    #[test]
    fn minting_is_idempotent_per_identity() {
        let home = tempfile::tempdir().unwrap();
        let first = mint(home.path(), &dan()).unwrap();
        let second = mint(home.path(), &dan()).unwrap();
        assert!(first.newly_minted);
        assert!(!second.newly_minted);
        assert_eq!(first.code, second.code);
    }

    #[test]
    fn codes_are_six_alphanumeric_chars() {
        let home = tempfile::tempdir().unwrap();
        let minted = mint(home.path(), &dan()).unwrap();
        assert_eq!(minted.code.len(), 6);
        assert!(minted.code.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn check_distinguishes_wrong_from_unminted() {
        let home = tempfile::tempdir().unwrap();
        let minted = mint(home.path(), &dan()).unwrap();
        assert_eq!(
            check(home.path(), "dan@example.com", &minted.code).unwrap(),
            CodeCheck::Valid
        );
        assert_eq!(
            check(home.path(), "dan@example.com", "nope!!").unwrap(),
            CodeCheck::WrongCode
        );
        assert_eq!(
            check(home.path(), "ghost@example.com", "nope!!").unwrap(),
            CodeCheck::NoCodeOnFile
        );
    }

    #[test]
    fn store_round_trips_member_kind() {
        let home = tempfile::tempdir().unwrap();
        mint(
            home.path(),
            &Member::agent("Claude Code", "claude-code@anthropic.com"),
        )
        .unwrap();
        let members = minted_members(home.path()).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(
            members[0].member,
            Member::agent("Claude Code", "claude-code@anthropic.com")
        );
    }
}
